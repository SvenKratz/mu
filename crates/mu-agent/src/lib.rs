#![allow(missing_docs)]
#![allow(unused_crate_dependencies)]

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use mu_ai::{
    AssistantEvent, ChatProvider, ContentPart, Message, ModelSpec, StopReason, StreamRequest,
    ToolCall, Usage,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{broadcast, Mutex};

pub mod instructions;
pub mod kanban;
pub mod session;
pub mod tools;

pub use instructions::{load_instruction_files, render_instruction_text, InstructionFile};
pub use session::{list_session_files, SessionEntry, SessionStore};
pub use kanban::document::{DocumentState, KanbanDocument};
pub use kanban::state::KanbanState;
pub use kanban::stats::KanbanStats;
pub use kanban::{KanbanCommand, KanbanEvent, KanbanRunner};
pub use tools::{default_tools, kanban_tools, AgentTool, ToolContext, ToolOutput};

#[derive(Debug, Error)]
pub enum MuAgentError {
    #[error(transparent)]
    Ai(#[from] mu_ai::MuAiError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
    #[error("tool {0} not found")]
    ToolNotFound(String),
    #[error("agent exceeded maximum turn count")]
    MaxTurnsExceeded,
    #[error("invalid state: {0}")]
    InvalidState(String),
}

#[derive(Clone)]
pub struct AgentConfig {
    pub system_prompt: String,
    pub model: ModelSpec,
    pub provider: Arc<dyn ChatProvider>,
    pub tools: Vec<Arc<dyn AgentTool>>,
    pub working_directory: PathBuf,
    pub session_store: SessionStore,
    pub max_turns: usize,
    pub auto_compact_threshold: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: ModelSpec,
    pub messages: Vec<Message>,
    pub is_streaming: bool,
    pub pending_tool_calls: Vec<String>,
    pub queued_steering: usize,
    pub queued_follow_up: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueMode {
    Steering,
    FollowUp,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentStart {
        input: Option<String>,
    },
    TurnStart {
        turn: usize,
    },
    TextDelta {
        delta: String,
    },
    ToolCallDelta {
        id: String,
        name: Option<String>,
        partial_json: String,
    },
    ToolCall {
        call: ToolCall,
    },
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        result: String,
        is_error: bool,
    },
    Usage {
        usage: Usage,
    },
    MessageComplete {
        message: Message,
    },
    TurnEnd {
        turn: usize,
    },
    QueueUpdated {
        steering: usize,
        follow_up: usize,
    },
    Compaction {
        summary: String,
    },
    AgentEnd {
        total_messages: usize,
    },
}

#[derive(Default)]
struct MessageQueues {
    steering: VecDeque<String>,
    follow_up: VecDeque<String>,
}

pub struct Agent {
    config: AgentConfig,
    state: Mutex<AgentState>,
    session_cursor: Mutex<Option<String>>,
    queues: Mutex<MessageQueues>,
    events: broadcast::Sender<AgentEvent>,
    tools_by_name: HashMap<String, Arc<dyn AgentTool>>,
}

impl Agent {
    pub async fn new(config: AgentConfig) -> Result<Self, MuAgentError> {
        let session_entries = config.session_store.load_entries()?;
        let (messages, cursor) = if let Some(last) = session_entries.last() {
            let branch = config.session_store.branch_to(&last.id)?;
            (
                branch.into_iter().map(|entry| entry.message).collect(),
                Some(last.id.clone()),
            )
        } else {
            (Vec::new(), None)
        };
        let (events, _) = broadcast::channel(256);
        let tools_by_name = config
            .tools
            .iter()
            .map(|tool| (tool.spec().name.clone(), Arc::clone(tool)))
            .collect::<HashMap<_, _>>();
        Ok(Self {
            state: Mutex::new(AgentState {
                system_prompt: config.system_prompt.clone(),
                model: config.model.clone(),
                messages,
                is_streaming: false,
                pending_tool_calls: Vec::new(),
                queued_steering: 0,
                queued_follow_up: 0,
            }),
            session_cursor: Mutex::new(cursor),
            queues: Mutex::new(MessageQueues::default()),
            events,
            tools_by_name,
            config,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }

    pub async fn state(&self) -> AgentState {
        self.state.lock().await.clone()
    }

    pub fn session_store(&self) -> &SessionStore {
        &self.config.session_store
    }

    pub fn working_directory(&self) -> &Path {
        &self.config.working_directory
    }

    pub async fn set_model(&self, model: ModelSpec) {
        let mut state = self.state.lock().await;
        state.model = model;
    }

    pub async fn reset(&self) {
        let mut state = self.state.lock().await;
        state.messages.clear();
        state.pending_tool_calls.clear();
        drop(state);
        let mut cursor = self.session_cursor.lock().await;
        *cursor = None;
    }

    pub async fn queue_message(&self, mode: QueueMode, input: impl Into<String>) {
        let input = input.into();
        let mut queues = self.queues.lock().await;
        match mode {
            QueueMode::Steering => queues.steering.push_back(input),
            QueueMode::FollowUp => queues.follow_up.push_back(input),
        }
        let steering = queues.steering.len();
        let follow_up = queues.follow_up.len();
        drop(queues);
        self.update_queue_counts(steering, follow_up).await;
    }

    pub async fn prompt(&self, input: impl Into<String>) -> Result<Message, MuAgentError> {
        self.run(Some(input.into())).await
    }

    pub async fn continue_from_current(&self) -> Result<Message, MuAgentError> {
        let state = self.state.lock().await;
        let Some(last) = state.messages.last() else {
            return Err(MuAgentError::InvalidState(
                "cannot continue without prior messages".to_string(),
            ));
        };
        if !matches!(last.role, mu_ai::Role::User | mu_ai::Role::Tool) {
            return Err(MuAgentError::InvalidState(
                "continue requires the last message to be user or tool".to_string(),
            ));
        }
        drop(state);
        self.run(None).await
    }

    pub async fn compact(&self, note: Option<String>) -> Result<Option<String>, MuAgentError> {
        let mut state = self.state.lock().await;
        let summary = compact_messages(&state.messages, note.as_deref());
        if let Some(summary) = summary {
            let keep_from = state.messages.len().saturating_sub(4);
            let mut new_messages = vec![Message::text(
                mu_ai::Role::System,
                format!("Conversation summary:\n{summary}"),
            )];
            new_messages.extend(state.messages.iter().skip(keep_from).cloned());
            state.messages = new_messages;
            let _ = self.events.send(AgentEvent::Compaction {
                summary: summary.clone(),
            });
            return Ok(Some(summary));
        }
        Ok(None)
    }

    pub async fn branch_to(&self, node_id: &str) -> Result<(), MuAgentError> {
        let branch = self.config.session_store.branch_to(node_id)?;
        let messages = branch
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>();
        let mut state = self.state.lock().await;
        state.messages = messages;
        drop(state);
        let mut cursor = self.session_cursor.lock().await;
        *cursor = Some(node_id.to_string());
        Ok(())
    }

    pub async fn session_tree(&self) -> Result<Vec<SessionEntry>, MuAgentError> {
        self.config.session_store.load_entries()
    }

    async fn run(&self, input: Option<String>) -> Result<Message, MuAgentError> {
        let _ = self.events.send(AgentEvent::AgentStart {
            input: input.clone(),
        });
        let mut pending_input = input;
        let mut turn = 0usize;
        loop {
            if turn >= self.config.max_turns {
                return Err(MuAgentError::MaxTurnsExceeded);
            }
            turn += 1;

            if let Some(input) = pending_input.take() {
                let user_message = Message::text(mu_ai::Role::User, input);
                self.append_message(user_message).await?;
            }

            let _ = self.events.send(AgentEvent::TurnStart { turn });
            let assistant_message = self.run_turn().await?;
            let tool_calls = assistant_message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::ToolCall(call) => Some(call.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();

            let assistant_message = self.append_message(assistant_message).await?;
            if tool_calls.is_empty() {
                let _ = self.events.send(AgentEvent::TurnEnd { turn });
                if let Some(next) = self.pop_next_queued_message().await {
                    pending_input = Some(next);
                    continue;
                }
                let total_messages = self.state.lock().await.messages.len();
                let _ = self.events.send(AgentEvent::AgentEnd { total_messages });
                return Ok(assistant_message);
            }

            let steering_waiting = self.execute_tool_calls(tool_calls).await?;
            let _ = self.events.send(AgentEvent::TurnEnd { turn });

            let queued_next = if let Some(steering_message) = steering_waiting {
                Some(steering_message)
            } else {
                self.pop_next_queued_message_now().await
            };

            if let Some(next) = queued_next {
                pending_input = Some(next);
                continue;
            }

            pending_input = None;
            self.auto_compact_if_needed().await?;
        }
    }

    async fn run_turn(&self) -> Result<Message, MuAgentError> {
        let request = {
            let mut state = self.state.lock().await;
            state.is_streaming = true;
            StreamRequest {
                model: state.model.clone(),
                messages: prepend_system_prompt(&state.system_prompt, &state.messages),
                tools: self.config.tools.iter().map(|tool| tool.spec()).collect(),
                max_tokens: Some(state.model.max_output_tokens),
                temperature: Some(0.0),
            }
        };

        let stream = self.config.provider.stream(request).await?;
        let mut text = String::new();
        let mut tool_calls = Vec::new();
        tokio::pin!(stream);

        while let Some(event) = stream.next().await {
            match event? {
                AssistantEvent::TextDelta { delta } => {
                    text.push_str(&delta);
                    let _ = self.events.send(AgentEvent::TextDelta { delta });
                }
                AssistantEvent::ToolCallDelta {
                    id,
                    name,
                    partial_json,
                } => {
                    let _ = self.events.send(AgentEvent::ToolCallDelta {
                        id,
                        name,
                        partial_json,
                    });
                }
                AssistantEvent::ToolCall { call } => {
                    tool_calls.push(call.clone());
                    {
                        let mut state = self.state.lock().await;
                        state.pending_tool_calls.push(call.id.clone());
                    }
                    let _ = self.events.send(AgentEvent::ToolCall { call });
                }
                AssistantEvent::Usage { usage } => {
                    let _ = self.events.send(AgentEvent::Usage { usage });
                }
                AssistantEvent::Stop { reason } => {
                    if matches!(reason, StopReason::Cancelled) {
                        break;
                    }
                }
            }
        }

        let mut state = self.state.lock().await;
        state.is_streaming = false;
        state.pending_tool_calls.clear();
        drop(state);

        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(ContentPart::Text { text });
        }
        content.extend(tool_calls.into_iter().map(ContentPart::ToolCall));
        Ok(Message {
            role: mu_ai::Role::Assistant,
            content,
            name: None,
            tool_call_id: None,
        })
    }

    async fn execute_tool_calls(
        &self,
        tool_calls: Vec<ToolCall>,
    ) -> Result<Option<String>, MuAgentError> {
        for call in tool_calls {
            let Some(tool) = self.tools_by_name.get(&call.name) else {
                return Err(MuAgentError::ToolNotFound(call.name));
            };

            let output = tool
                .run(
                    call.arguments.clone(),
                    ToolContext {
                        working_directory: self.config.working_directory.clone(),
                    },
                )
                .await?;
            let message = Message::with_tool_result(
                call.id.clone(),
                call.name.clone(),
                output.content.clone(),
                output.is_error,
            );
            self.append_message(message).await?;
            let _ = self.events.send(AgentEvent::ToolResult {
                tool_call_id: call.id,
                tool_name: call.name,
                result: output.content,
                is_error: output.is_error,
            });

            let steering = {
                let mut queues = self.queues.lock().await;
                let value = queues.steering.pop_front();
                let steering = queues.steering.len();
                let follow_up = queues.follow_up.len();
                drop(queues);
                self.update_queue_counts(steering, follow_up).await;
                value
            };
            if steering.is_some() {
                return Ok(steering);
            }
        }

        Ok(None)
    }

    async fn append_message(&self, message: Message) -> Result<Message, MuAgentError> {
        {
            let mut state = self.state.lock().await;
            state.messages.push(message.clone());
        }
        let parent_id = self.session_cursor.lock().await.clone();
        let entry = self.config.session_store.append(parent_id, &message)?;
        let mut cursor = self.session_cursor.lock().await;
        *cursor = Some(entry.id);
        let _ = self.events.send(AgentEvent::MessageComplete {
            message: message.clone(),
        });
        Ok(message)
    }

    async fn pop_next_queued_message(&self) -> Option<String> {
        let next = self.pop_next_queued_message_now().await;
        self.auto_compact_if_needed().await.ok()?;
        next
    }

    async fn pop_next_queued_message_now(&self) -> Option<String> {
        let mut queues = self.queues.lock().await;
        let next = queues
            .steering
            .pop_front()
            .or_else(|| queues.follow_up.pop_front());
        let steering = queues.steering.len();
        let follow_up = queues.follow_up.len();
        drop(queues);
        self.update_queue_counts(steering, follow_up).await;
        next
    }

    async fn update_queue_counts(&self, steering: usize, follow_up: usize) {
        {
            let mut state = self.state.lock().await;
            state.queued_steering = steering;
            state.queued_follow_up = follow_up;
        }
        let _ = self.events.send(AgentEvent::QueueUpdated {
            steering,
            follow_up,
        });
    }

    async fn auto_compact_if_needed(&self) -> Result<(), MuAgentError> {
        let should_compact = {
            let state = self.state.lock().await;
            state.messages.len() > self.config.auto_compact_threshold
        };
        if should_compact {
            let _ = self.compact(None).await?;
        }
        Ok(())
    }
}

fn prepend_system_prompt(system_prompt: &str, messages: &[Message]) -> Vec<Message> {
    let mut all_messages = vec![Message::text(
        mu_ai::Role::System,
        system_prompt.to_string(),
    )];
    all_messages.extend(messages.iter().cloned());
    all_messages
}

fn compact_messages(messages: &[Message], note: Option<&str>) -> Option<String> {
    if messages.len() < 6 {
        return None;
    }

    let take = messages.len().saturating_sub(4);
    let mut lines = Vec::new();
    if let Some(note) = note {
        lines.push(format!("Compaction note: {note}"));
    }
    for message in messages.iter().take(take) {
        let role = match message.role {
            mu_ai::Role::System => "system",
            mu_ai::Role::User => "user",
            mu_ai::Role::Assistant => "assistant",
            mu_ai::Role::Tool => "tool",
        };
        let text = message.plain_text();
        if !text.is_empty() {
            lines.push(format!("{role}: {text}"));
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;

    use async_trait::async_trait;
    use mu_ai::{
        AssistantEvent, AssistantEventStream, ChatProvider, Message, ModelSpec, ProviderId,
        StreamRequest,
    };
    use serde_json::json;
    use tempfile::TempDir;

    use super::{default_tools, Agent, AgentConfig, MuAgentError, QueueMode, SessionStore};

    struct ScriptedProvider {
        responses: tokio::sync::Mutex<VecDeque<Vec<AssistantEvent>>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<Vec<AssistantEvent>>) -> Self {
            Self {
                responses: tokio::sync::Mutex::new(VecDeque::from(responses)),
            }
        }
    }

    #[async_trait]
    impl ChatProvider for ScriptedProvider {
        async fn stream(
            &self,
            _request: StreamRequest,
        ) -> Result<AssistantEventStream, mu_ai::MuAiError> {
            let mut responses = self.responses.lock().await;
            let Some(events) = responses.pop_front() else {
                return Err(mu_ai::MuAiError::Provider(
                    "no scripted response left".to_string(),
                ));
            };
            Ok(Box::pin(futures::stream::iter(
                events.into_iter().map(Ok::<_, mu_ai::MuAiError>),
            )))
        }
    }

    fn model() -> ModelSpec {
        ModelSpec::new(
            ProviderId::OpenAiCompatible,
            "gpt-4o-mini",
            "GPT-4o mini",
            128_000,
            16_384,
        )
    }

    async fn build_agent(
        tempdir: &TempDir,
        responses: Vec<Vec<AssistantEvent>>,
    ) -> Result<Agent, MuAgentError> {
        Agent::new(AgentConfig {
            system_prompt: "You are Mu.".to_string(),
            model: model(),
            provider: Arc::new(ScriptedProvider::new(responses)),
            tools: default_tools(tempdir.path()),
            working_directory: tempdir.path().to_path_buf(),
            session_store: SessionStore::from_path(tempdir.path().join("session.jsonl")),
            max_turns: 6,
            auto_compact_threshold: 100,
        })
        .await
    }

    #[tokio::test]
    async fn handles_single_turn_prompt() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let agent = match build_agent(
            &tempdir,
            vec![vec![
                AssistantEvent::TextDelta {
                    delta: "hello".to_string(),
                },
                AssistantEvent::Stop {
                    reason: mu_ai::StopReason::EndTurn,
                },
            ]],
        )
        .await
        {
            Ok(value) => value,
            Err(error) => panic!("agent should build: {error}"),
        };

        let response = match agent.prompt("hi").await {
            Ok(value) => value,
            Err(error) => panic!("prompt should succeed: {error}"),
        };

        assert_eq!(response.plain_text(), "hello");
        let state = agent.state().await;
        assert_eq!(state.messages.len(), 2);
    }

    #[tokio::test]
    async fn executes_tools_and_continues() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let agent = match build_agent(
            &tempdir,
            vec![
                vec![
                    AssistantEvent::ToolCall {
                        call: mu_ai::ToolCall {
                            id: "call_1".to_string(),
                            name: "write".to_string(),
                            arguments: json!({
                                "path": "note.txt",
                                "content": "hello"
                            }),
                        },
                    },
                    AssistantEvent::Stop {
                        reason: mu_ai::StopReason::ToolCall,
                    },
                ],
                vec![
                    AssistantEvent::TextDelta {
                        delta: "done".to_string(),
                    },
                    AssistantEvent::Stop {
                        reason: mu_ai::StopReason::EndTurn,
                    },
                ],
            ],
        )
        .await
        {
            Ok(value) => value,
            Err(error) => panic!("agent should build: {error}"),
        };

        let response = match agent.prompt("write note").await {
            Ok(value) => value,
            Err(error) => panic!("prompt should succeed: {error}"),
        };

        assert_eq!(response.plain_text(), "done");
        let written = match std::fs::read_to_string(tempdir.path().join("note.txt")) {
            Ok(value) => value,
            Err(error) => panic!("file should exist: {error}"),
        };
        assert_eq!(written, "hello");
    }

    #[tokio::test]
    async fn prioritizes_steering_before_follow_up() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let agent = match build_agent(
            &tempdir,
            vec![
                vec![
                    AssistantEvent::ToolCall {
                        call: mu_ai::ToolCall {
                            id: "call_1".to_string(),
                            name: "write".to_string(),
                            arguments: json!({
                                "path": "one.txt",
                                "content": "1"
                            }),
                        },
                    },
                    AssistantEvent::ToolCall {
                        call: mu_ai::ToolCall {
                            id: "call_2".to_string(),
                            name: "write".to_string(),
                            arguments: json!({
                                "path": "two.txt",
                                "content": "2"
                            }),
                        },
                    },
                    AssistantEvent::Stop {
                        reason: mu_ai::StopReason::ToolCall,
                    },
                ],
                vec![
                    AssistantEvent::TextDelta {
                        delta: "steered".to_string(),
                    },
                    AssistantEvent::Stop {
                        reason: mu_ai::StopReason::EndTurn,
                    },
                ],
                vec![
                    AssistantEvent::TextDelta {
                        delta: "followed".to_string(),
                    },
                    AssistantEvent::Stop {
                        reason: mu_ai::StopReason::EndTurn,
                    },
                ],
            ],
        )
        .await
        {
            Ok(value) => value,
            Err(error) => panic!("agent should build: {error}"),
        };

        agent
            .queue_message(QueueMode::Steering, "change course")
            .await;
        agent.queue_message(QueueMode::FollowUp, "after that").await;

        let response = match agent.prompt("start").await {
            Ok(value) => value,
            Err(error) => panic!("prompt should succeed: {error}"),
        };

        assert_eq!(response.plain_text(), "followed");
        assert!(tempdir.path().join("one.txt").exists());
        assert!(!tempdir.path().join("two.txt").exists());
    }

    #[test]
    fn persists_sessions_as_jsonl() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let store = SessionStore::from_path(tempdir.path().join("session.jsonl"));
        let first = match store.append(None, &Message::text(mu_ai::Role::User, "hello")) {
            Ok(value) => value,
            Err(error) => panic!("append should succeed: {error}"),
        };
        let second = match store.append(
            Some(first.id.clone()),
            &Message::text(mu_ai::Role::Assistant, "world"),
        ) {
            Ok(value) => value,
            Err(error) => panic!("append should succeed: {error}"),
        };

        let entries = match store.load_entries() {
            Ok(value) => value,
            Err(error) => panic!("load should succeed: {error}"),
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].parent_id, Some(first.id));
        assert_eq!(entries[1].id, second.id);
    }

    #[test]
    fn branches_and_reconstructs_paths() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let store = SessionStore::from_path(tempdir.path().join("session.jsonl"));
        let root = match store.append(None, &Message::text(mu_ai::Role::User, "hello")) {
            Ok(value) => value,
            Err(error) => panic!("append should succeed: {error}"),
        };
        let branch_a = match store.append(
            Some(root.id.clone()),
            &Message::text(mu_ai::Role::Assistant, "a"),
        ) {
            Ok(value) => value,
            Err(error) => panic!("append should succeed: {error}"),
        };
        let branch_b = match store.append(
            Some(root.id.clone()),
            &Message::text(mu_ai::Role::Assistant, "b"),
        ) {
            Ok(value) => value,
            Err(error) => panic!("append should succeed: {error}"),
        };

        let a_path = match store.branch_to(&branch_a.id) {
            Ok(value) => value,
            Err(error) => panic!("branch should succeed: {error}"),
        };
        let b_path = match store.branch_to(&branch_b.id) {
            Ok(value) => value,
            Err(error) => panic!("branch should succeed: {error}"),
        };
        assert_eq!(a_path.len(), 2);
        assert_eq!(b_path.len(), 2);
        assert_eq!(a_path[1].message.plain_text(), "a");
        assert_eq!(b_path[1].message.plain_text(), "b");
    }

    #[tokio::test]
    async fn compacts_long_histories() {
        let tempdir = match TempDir::new() {
            Ok(value) => value,
            Err(error) => panic!("tempdir should exist: {error}"),
        };
        let agent = match build_agent(
            &tempdir,
            vec![vec![
                AssistantEvent::TextDelta {
                    delta: "hello".to_string(),
                },
                AssistantEvent::Stop {
                    reason: mu_ai::StopReason::EndTurn,
                },
            ]],
        )
        .await
        {
            Ok(value) => value,
            Err(error) => panic!("agent should build: {error}"),
        };

        {
            let mut state = agent.state.lock().await;
            state.messages = vec![
                Message::text(mu_ai::Role::User, "one"),
                Message::text(mu_ai::Role::Assistant, "two"),
                Message::text(mu_ai::Role::User, "three"),
                Message::text(mu_ai::Role::Assistant, "four"),
                Message::text(mu_ai::Role::User, "five"),
                Message::text(mu_ai::Role::Assistant, "six"),
            ];
        }

        let summary = match agent.compact(Some("keep intent".to_string())).await {
            Ok(Some(value)) => value,
            Ok(None) => panic!("expected compaction summary"),
            Err(error) => panic!("compaction should succeed: {error}"),
        };
        assert!(summary.contains("keep intent"));
        let state = agent.state().await;
        assert!(
            matches!(state.messages.first(), Some(message) if message.plain_text().contains("Conversation summary"))
        );
    }
}
