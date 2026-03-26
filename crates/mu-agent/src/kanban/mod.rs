pub mod document;
pub mod logger;
pub mod state;
pub mod stats;
pub mod watcher;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use self::document::{parse_kanban_filename, parse_preamble, DocumentState, KanbanDocument};
use self::logger::KanbanLogger;
use self::state::KanbanState;
use self::stats::KanbanStats;
use self::watcher::KanbanWatcher;
use crate::{Agent, AgentConfig, MuAgentError, SessionEntry, SessionStore};
use mu_ai::{ContentPart, Role};

/// Rename a file with path context in the error message.
fn fs_rename(from: &Path, to: &Path) -> Result<(), MuAgentError> {
    std::fs::rename(from, to).map_err(|e| {
        MuAgentError::io_path(e, format!("rename {} -> {}", from.display(), to.display()))
    })
}

/// Write a file with path context in the error message.
fn fs_write(path: &Path, content: &str) -> Result<(), MuAgentError> {
    std::fs::write(path, content).map_err(|e| MuAgentError::io_path(e, path.display()))
}

/// Create directories with path context in the error message.
fn fs_mkdir(path: &Path) -> Result<(), MuAgentError> {
    std::fs::create_dir_all(path).map_err(|e| MuAgentError::io_path(e, path.display()))
}

const KANBAN_SYSTEM_PROMPT: &str = r#"You are Mu, a pragmatic coding agent working in kanban mode.

You are processing a task document from a kanban board. The document content below is your task specification.

**Working directory**: Your working directory is set for this specific task. All files you create will be deposited there.

**Available tools**: read, write, edit, bash, request_feedback, create_task.

## Deciding whether to decompose or implement

**Decompose** when the task is:
- A product brief, feature spec, or high-level project description
- Multi-step work requiring multiple files, components, or subsystems
- Too large or complex to implement well in a single agent session

**Implement directly** when the task is:
- A concrete, focused implementation task (e.g. "build the login form component")
- A bug fix, refactor, or single-file change
- Already scoped to a clear deliverable

## Task decomposition (when applicable)

If the task should be decomposed:

1. Analyze the task and identify the concrete implementation steps
2. Use the `create_task` tool to create each subtask as a new kanban document
3. Use frontmatter to wire up dependencies and shared context:
   ```
   ---
   task_id: unique-name
   project_id: shared-project-id
   depends_on: earlier-task-id, another-task-id
   work_dir: /path/to/target/codebase
   ---
   Concrete implementation instructions here...
   ```
4. Key rules for subtasks:
   - Give each subtask a unique `task_id` so other tasks can depend on it
   - Use `depends_on` to declare ordering (tasks only run when all deps are complete)
   - Use the same `project_id` on all subtasks so they share a working directory
   - Propagate `work_dir` from the parent task if one was specified
   - Write specific, actionable instructions — not vague goals
   - Independent subtasks will be executed in parallel automatically
5. Create a final integration/verification task that `depends_on` all implementation tasks
6. After creating subtasks, summarize what you created and stop — do NOT implement them yourself

## Direct implementation (when applicable)

If the task is concrete enough to implement directly:
- Work methodically through the task
- Create output files in your working directory
- If you need clarification, use `request_feedback`

## Conventions
- Be thorough but concise
- If the task is unclear, request feedback rather than guessing
- When complete, provide a brief summary of what was accomplished
"#;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KanbanEvent {
    DocumentDiscovered {
        id: String,
        name: String,
    },
    StateChanged {
        id: String,
        from: String,
        to: String,
    },
    ProcessingStarted {
        id: String,
    },
    ProcessingComplete {
        id: String,
    },
    FeedbackRequested {
        id: String,
        question: String,
    },
    StatsUpdated(KanbanStats),
    Error {
        id: Option<String>,
        message: String,
    },
    StatusResponse {
        documents: Vec<KanbanDocumentSummary>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct KanbanDocumentSummary {
    pub name: String,
    pub state: String,
    pub elapsed_secs: Option<i64>,
    pub error: Option<String>,
}

/// Commands that can be sent to the KanbanRunner from external sources (e.g. web UI).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KanbanCommand {
    /// Cancel a document: moves Todo/Processing/Error → Draft
    CancelDocument { id: String },
    /// Create a new draft document
    CreateDraft {
        name: String,
        content: String,
        work_dir: Option<String>,
    },
    /// Submit a draft document: moves Draft → Todo
    SubmitDocument { id: String },
    /// Retry an errored document: moves Error → Todo
    RetryDocument { id: String },
    /// Create a new document directly in TODO (skipping draft)
    CreateTodo {
        name: String,
        content: String,
        work_dir: Option<String>,
    },
    /// Reload state from disk (used when external code writes directly)
    ReloadState,
    /// Retry all errored documents: moves Error → Todo for each
    RetryAllErrored,
    /// Request a status snapshot of all documents
    RequestStatus,
}

pub struct KanbanRunner {
    state: KanbanState,
    watcher: KanbanWatcher,
    config_template: AgentConfig,
    stats: KanbanStats,
    event_tx: broadcast::Sender<KanbanEvent>,
    command_rx: mpsc::Receiver<KanbanCommand>,
    logger: KanbanLogger,
}

/// Pre-computed data needed to run an agent task, extracted during the
/// sequential preparation phase so the expensive agent work can be spawned.
struct TaskPrep {
    doc_id: String,
    original_name: String,
    processing_file: PathBuf,
    result_dir: PathBuf,
    session_dir: PathBuf,
    prompt: String,
    config: AgentConfig,
}

/// The result of running an agent task, applied back to kanban state
/// during the sequential finalization phase.
enum TaskOutcome {
    Complete {
        doc_id: String,
        original_name: String,
        processing_file: PathBuf,
        session_dir: PathBuf,
    },
    FeedbackRequested {
        doc_id: String,
        processing_file: PathBuf,
        question: String,
    },
    Error {
        doc_id: String,
        processing_file: PathBuf,
        error: String,
    },
}

/// Run an agent task to completion and return the outcome.
/// This is a free function so it can be sent to `tokio::spawn`.
async fn run_agent_task(prep: TaskPrep) -> TaskOutcome {
    let agent = match Agent::new(prep.config).await {
        Ok(agent) => agent,
        Err(err) => {
            return TaskOutcome::Error {
                doc_id: prep.doc_id,
                processing_file: prep.processing_file,
                error: err.to_string(),
            };
        }
    };

    match agent.prompt(prep.prompt).await {
        Ok(_message) => {
            let feedback_request = prep.result_dir.join("feedback_request.md");
            if feedback_request.exists() {
                let question = std::fs::read_to_string(&feedback_request)
                    .unwrap_or_else(|_| "Feedback requested".to_string());
                TaskOutcome::FeedbackRequested {
                    doc_id: prep.doc_id,
                    processing_file: prep.processing_file,
                    question,
                }
            } else {
                TaskOutcome::Complete {
                    doc_id: prep.doc_id,
                    original_name: prep.original_name,
                    processing_file: prep.processing_file,
                    session_dir: prep.session_dir,
                }
            }
        }
        Err(err) => TaskOutcome::Error {
            doc_id: prep.doc_id,
            processing_file: prep.processing_file,
            error: err.to_string(),
        },
    }
}

impl KanbanRunner {
    pub fn new(
        root: PathBuf,
        config_template: AgentConfig,
    ) -> Result<
        (
            Self,
            broadcast::Receiver<KanbanEvent>,
            broadcast::Sender<KanbanEvent>,
            mpsc::Sender<KanbanCommand>,
        ),
        MuAgentError,
    > {
        let state = KanbanState::load_or_create(root.clone())?;
        state.ensure_folders()?;
        let watcher = KanbanWatcher::new(&root)?;
        let stats = KanbanStats::from_state(&state);
        let logger = KanbanLogger::new(root.join("logs").join("kanban.jsonl"))?;
        let (event_tx, event_rx) = broadcast::channel(256);
        let (command_tx, command_rx) = mpsc::channel(64);

        Ok((
            Self {
                state,
                watcher,
                config_template,
                stats,
                event_tx: event_tx.clone(),
                command_rx,
                logger,
            },
            event_rx,
            event_tx,
            command_tx,
        ))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<KanbanEvent> {
        self.event_tx.subscribe()
    }

    pub fn stats(&self) -> &KanbanStats {
        &self.stats
    }

    /// Access the current kanban state (for API endpoints).
    pub fn state(&self) -> &KanbanState {
        &self.state
    }

    /// Broadcast an event and write it to the structured log.
    fn emit(&mut self, event: KanbanEvent) {
        self.logger.log_event(&event, &self.state);
        let _ = self.event_tx.send(event);
    }

    pub async fn run(&mut self) -> Result<(), MuAgentError> {
        self.state.ensure_folders()?;
        self.scan_and_reconcile()?;
        self.drain_commands()?;
        self.dispatch_pending_work().await?;
        self.update_stats()?;

        loop {
            tokio::select! {
                Some(_event) = self.watcher.next_event() => {
                    // Drain any additional queued events
                    self.watcher.drain();
                    self.scan_and_reconcile()?;
                }
                Some(cmd) = self.command_rx.recv() => {
                    self.handle_command(cmd)?;
                }
                _ = tokio::time::sleep(Duration::from_secs(2)) => {
                    self.scan_and_reconcile()?;
                }
            }
            // Drain any commands that arrived while dispatch was running
            self.drain_commands()?;
            self.dispatch_pending_work().await?;
            self.update_stats()?;
        }
    }

    /// Process all pending commands without blocking.
    fn drain_commands(&mut self) -> Result<(), MuAgentError> {
        while let Ok(cmd) = self.command_rx.try_recv() {
            self.handle_command(cmd)?;
        }
        Ok(())
    }

    /// Handle a command from an external source (e.g. web UI).
    fn handle_command(&mut self, cmd: KanbanCommand) -> Result<(), MuAgentError> {
        self.logger.log_command(&cmd, &self.state);
        match cmd {
            KanbanCommand::CancelDocument { id } => {
                let doc = match self.state.get_document(&id) {
                    Some(doc) => doc.clone(),
                    None => return Ok(()),
                };

                let from = doc.state.to_string();

                // Processing → Todo (requeue), others → Draft
                let (target_state, target_folder) =
                    if doc.state == DocumentState::Processing {
                        (DocumentState::Todo, self.state.todo_path())
                    } else if doc.state.can_transition_to(&DocumentState::Draft) {
                        (DocumentState::Draft, self.state.draft_path())
                    } else {
                        return Ok(());
                    };

                let source_folder = doc.state.folder_name();
                let source_file = self
                    .state
                    .folder_path(source_folder)
                    .join(format!("{}.md", doc.file_stem()));
                let dest_file = target_folder.join(format!("{}.md", doc.file_stem()));

                if source_file.exists() {
                    fs_rename(&source_file, &dest_file)?;
                }

                let to = target_state.to_string();
                if let Some(d) = self.state.get_document_mut(&id) {
                    d.transition_to(target_state);
                    d.error = None;
                }
                self.state.save()?;

                self.emit(KanbanEvent::StateChanged { id, from, to });
            }
            KanbanCommand::CreateDraft { name, content, work_dir } => {
                let id = Uuid::now_v7().to_string();
                let mut doc = KanbanDocument::new(id.clone(), name.clone());
                doc.state = DocumentState::Draft;
                doc.work_dir = work_dir.map(PathBuf::from);

                // Parse preamble for metadata (frontmatter is optional)
                let (preamble, _body) = parse_preamble(&content);
                doc.task_id = preamble.task_id;
                doc.depends_on = preamble.depends_on;
                doc.persona = preamble.persona;
                // Default task_id to the task name so other tasks can depend on it
                if doc.task_id.is_none() {
                    doc.task_id = Some(name.clone());
                }
                if let Some(ref pid) = preamble.project_id {
                    doc.project_id = Some(pid.clone());
                }
                if doc.work_dir.is_none() {
                    if let Some(ref wd) = preamble.work_dir {
                        doc.work_dir = Some(PathBuf::from(wd));
                    }
                }

                let draft_file = self
                    .state
                    .draft_path()
                    .join(format!("{}.md", doc.file_stem()));
                fs_write(&draft_file, &content)?;

                self.state.insert_document(doc);
                self.state.save()?;

                self.emit(KanbanEvent::DocumentDiscovered {
                    id,
                    name,
                });
            }
            KanbanCommand::SubmitDocument { id } => {
                let doc = match self.state.get_document(&id) {
                    Some(doc) => doc.clone(),
                    None => return Ok(()),
                };
                if doc.state != DocumentState::Draft {
                    return Ok(());
                }

                // Move file from DRAFT/ to TODO/
                let draft_file = self
                    .state
                    .draft_path()
                    .join(format!("{}.md", doc.file_stem()));
                let todo_file = self
                    .state
                    .todo_path()
                    .join(format!("{}.md", doc.file_stem()));
                if draft_file.exists() {
                    fs_rename(&draft_file, &todo_file)?;
                }

                if let Some(d) = self.state.get_document_mut(&id) {
                    d.transition_to(DocumentState::Todo);
                }
                self.state.save()?;

                self.emit(KanbanEvent::StateChanged {
                    id,
                    from: "draft".to_string(),
                    to: "todo".to_string(),
                });
            }
            KanbanCommand::RetryDocument { id } => {
                let doc = match self.state.get_document(&id) {
                    Some(doc) => doc.clone(),
                    None => return Ok(()),
                };
                if doc.state != DocumentState::Error {
                    return Ok(());
                }

                // Move file to TODO/
                let error_file = self
                    .state
                    .folder_path(doc.state.folder_name())
                    .join(format!("{}.md", doc.file_stem()));
                let todo_file = self
                    .state
                    .todo_path()
                    .join(format!("{}.md", doc.file_stem()));
                if error_file.exists() {
                    fs_rename(&error_file, &todo_file)?;
                }

                if let Some(d) = self.state.get_document_mut(&id) {
                    d.error = None;
                    d.transition_to(DocumentState::Todo);
                }
                self.state.save()?;

                self.emit(KanbanEvent::StateChanged {
                    id,
                    from: "error".to_string(),
                    to: "todo".to_string(),
                });
            }
            KanbanCommand::CreateTodo { name, content, work_dir } => {
                let id = Uuid::now_v7().to_string();
                let mut doc = KanbanDocument::new(id.clone(), name.clone());
                // KanbanDocument::new already sets state to Todo
                doc.work_dir = work_dir.map(PathBuf::from);

                // Parse preamble for metadata (frontmatter is optional)
                let (preamble, _body) = parse_preamble(&content);
                doc.task_id = preamble.task_id;
                doc.depends_on = preamble.depends_on;
                doc.persona = preamble.persona;
                if doc.task_id.is_none() {
                    doc.task_id = Some(name.clone());
                }
                if let Some(ref pid) = preamble.project_id {
                    doc.project_id = Some(pid.clone());
                }
                if doc.work_dir.is_none() {
                    if let Some(ref wd) = preamble.work_dir {
                        doc.work_dir = Some(PathBuf::from(wd));
                    }
                }

                let todo_file = self
                    .state
                    .todo_path()
                    .join(format!("{}.md", doc.file_stem()));
                fs_write(&todo_file, &content)?;

                self.state.insert_document(doc);
                self.state.save()?;

                self.emit(KanbanEvent::DocumentDiscovered {
                    id,
                    name,
                });
            }
            KanbanCommand::ReloadState => {
                if let Ok(fresh) = KanbanState::load_or_create(self.state.root.clone()) {
                    self.state = fresh;
                }
            }
            KanbanCommand::RetryAllErrored => {
                let errored_ids: Vec<String> = self
                    .state
                    .documents
                    .values()
                    .filter(|d| d.state == DocumentState::Error)
                    .map(|d| d.id.clone())
                    .collect();
                for id in errored_ids {
                    let doc = match self.state.get_document(&id) {
                        Some(doc) => doc.clone(),
                        None => continue,
                    };
                    let error_file = self
                        .state
                        .folder_path(doc.state.folder_name())
                        .join(format!("{}.md", doc.file_stem()));
                    let todo_file = self
                        .state
                        .todo_path()
                        .join(format!("{}.md", doc.file_stem()));
                    if error_file.exists() {
                        fs_rename(&error_file, &todo_file)?;
                    }
                    if let Some(d) = self.state.get_document_mut(&id) {
                        d.error = None;
                        d.transition_to(DocumentState::Todo);
                    }
                    self.emit(KanbanEvent::StateChanged {
                        id,
                        from: "error".to_string(),
                        to: "todo".to_string(),
                    });
                }
                self.state.save()?;
            }
            KanbanCommand::RequestStatus => {
                let now = Utc::now();
                let documents: Vec<KanbanDocumentSummary> = self
                    .state
                    .documents
                    .values()
                    .filter(|d| d.state != DocumentState::Draft)
                    .map(|d| {
                        let elapsed_secs = if d.state == DocumentState::Processing {
                            Some(now.signed_duration_since(d.updated_at).num_seconds().max(0))
                        } else {
                            None
                        };
                        KanbanDocumentSummary {
                            name: d.original_name.clone(),
                            state: d.state.to_string(),
                            elapsed_secs,
                            error: d.error.clone(),
                        }
                    })
                    .collect();
                self.emit(KanbanEvent::StatusResponse { documents });
            }
        }
        Ok(())
    }

    /// Scan filesystem and reconcile with in-memory state.
    fn scan_and_reconcile(&mut self) -> Result<(), MuAgentError> {
        // Discover new .md files dropped into DRAFT/
        let draft_files = KanbanState::list_md_files(&self.state.draft_path())?;
        for path in draft_files {
            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            // Already tracked (has UUID in filename)
            if let Some((_name, id)) = parse_kanban_filename(&filename) {
                if self.state.get_document(&id).is_some() {
                    continue;
                }
            }

            // New untracked file in DRAFT — assign UUID and register as draft
            let original_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string();
            let id = Uuid::now_v7().to_string();
            let mut doc = KanbanDocument::new(id.clone(), original_name.clone());
            doc.state = DocumentState::Draft;

            if let Ok(content) = std::fs::read_to_string(&path) {
                let (preamble, _body) = parse_preamble(&content);
                doc.task_id = preamble.task_id;
                doc.depends_on = preamble.depends_on;
                doc.persona = preamble.persona;
                if let Some(ref pid) = preamble.project_id {
                    doc.project_id = Some(pid.clone());
                }
                if let Some(ref wd) = preamble.work_dir {
                    doc.work_dir = Some(PathBuf::from(wd));
                }
            }
            if doc.task_id.is_none() {
                doc.task_id = Some(original_name.clone());
            }

            let new_filename = format!("{}.md", doc.file_stem());
            let new_path = self.state.draft_path().join(&new_filename);

            fs_rename(&path, &new_path)?;
            self.state.insert_document(doc);
            self.state.save()?;

            self.emit(KanbanEvent::DocumentDiscovered {
                id,
                name: original_name,
            });
        }

        // Discover new .md files in TODO/
        let todo_files = KanbanState::list_md_files(&self.state.todo_path())?;
        for path in todo_files {
            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            // Check if this is already a kanban-named file (has UUID)
            if let Some((name, id)) = parse_kanban_filename(&filename) {
                if let Some(doc) = self.state.get_document(&id) {
                    // File is in TODO/ but state disagrees — reconcile
                    if doc.state == DocumentState::Draft {
                        let from = doc.state.to_string();
                        if let Some(d) = self.state.get_document_mut(&id) {
                            d.transition_to(DocumentState::Todo);
                            // Default task_id if missing
                            if d.task_id.is_none() {
                                d.task_id = Some(d.original_name.clone());
                            }
                        }
                        self.state.save()?;
                        self.emit(KanbanEvent::StateChanged {
                            id: id.clone(),
                            from,
                            to: "todo".to_string(),
                        });
                    }
                    continue; // Already tracked
                }

                // Check if the UUID references an existing completed document
                // (continuation: extend/modify that task's result project)
                let is_continuation = self
                    .state
                    .documents
                    .values()
                    .any(|d| d.id == id && d.state == DocumentState::Complete);

                if is_continuation {
                    let new_id = Uuid::now_v7().to_string();
                    let mut doc = KanbanDocument::new(new_id.clone(), name.clone());
                    doc.continues_from = Some(id.clone());
                    doc.project_id = Some(id);

                    // Parse preamble for additional metadata (frontmatter is optional)
                    if let Ok(content) = std::fs::read_to_string(&path) {
                        let (preamble, _body) = parse_preamble(&content);
                        doc.task_id = preamble.task_id;
                        doc.depends_on = preamble.depends_on;
                        doc.persona = preamble.persona;
                        if let Some(ref pid) = preamble.project_id {
                            doc.project_id = Some(pid.clone());
                        }
                        if let Some(ref wd) = preamble.work_dir {
                            doc.work_dir = Some(PathBuf::from(wd));
                        }
                    }
                    // Default task_id to the document name
                    if doc.task_id.is_none() {
                        doc.task_id = Some(name.clone());
                    }

                    let new_filename = format!("{}.md", doc.file_stem());
                    let new_path = self.state.todo_path().join(&new_filename);

                    fs_rename(&path, &new_path)?;
                    self.state.insert_document(doc);
                    self.state.save()?;

                    self.emit(KanbanEvent::DocumentDiscovered {
                        id: new_id,
                        name,
                    });
                    continue;
                }
            }

            // New document: assign ID, rename, and track
            let original_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("untitled")
                .to_string();
            let id = Uuid::now_v7().to_string();
            let mut doc = KanbanDocument::new(id.clone(), original_name.clone());

            // Parse preamble for metadata (frontmatter is optional)
            if let Ok(content) = std::fs::read_to_string(&path) {
                let (preamble, _body) = parse_preamble(&content);
                doc.task_id = preamble.task_id;
                doc.depends_on = preamble.depends_on;
                doc.persona = preamble.persona;
                if let Some(ref pid) = preamble.project_id {
                    doc.project_id = Some(pid.clone());
                }
                if let Some(ref wd) = preamble.work_dir {
                    doc.work_dir = Some(PathBuf::from(wd));
                }
            }
            // Default task_id to the filename-derived name
            if doc.task_id.is_none() {
                doc.task_id = Some(original_name.clone());
            }

            let new_filename = format!("{}.md", doc.file_stem());
            let new_path = self.state.todo_path().join(&new_filename);

            fs_rename(&path, &new_path)?;
            self.state.insert_document(doc);
            self.state.save()?;

            self.emit(KanbanEvent::DocumentDiscovered {
                id,
                name: original_name,
            });
        }

        // Check FEEDBACK/ for edited responses (user has responded)
        let feedback_files = KanbanState::list_md_files(&self.state.feedback_path())?;
        for path in feedback_files {
            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            if let Some((_name, id)) = parse_kanban_filename(&filename) {
                if let Some(doc) = self.state.get_document(&id) {
                    if doc.state == DocumentState::Feedback {
                        // Check if there's a user response file alongside
                        let response_file = self.state.feedback_path().join(format!(
                            "{}_response.md",
                            doc.file_stem()
                        ));
                        if response_file.exists() {
                            // Move back to PROCESSING
                            let processing_path =
                                self.state.processing_path().join(&filename);
                            fs_rename(&path, &processing_path)?;
                            // Also move the response file
                            let response_dest = self.state.processing_path().join(format!(
                                "{}_response.md",
                                doc.file_stem()
                            ));
                            fs_rename(&response_file, &response_dest)?;

                            let from = doc.state.to_string();
                            if let Some(doc) = self.state.get_document_mut(&id) {
                                doc.transition_to(DocumentState::Processing);
                            }
                            self.state.save()?;
                            self.emit(KanbanEvent::StateChanged {
                                id: id.clone(),
                                from,
                                to: "processing".to_string(),
                            });
                        }
                    }
                }
            }
        }

        // Check REFINE/ for _COMMENTS.md files
        let refine_files = KanbanState::list_md_files(&self.state.refine_path())?;
        for path in refine_files {
            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };

            // Look for files like `<stem>_COMMENTS.md`
            if let Some(stem) = filename.strip_suffix("_COMMENTS.md") {
                // Find the matching completed document
                let matching_doc = self
                    .state
                    .documents
                    .values()
                    .find(|doc| {
                        doc.file_stem() == stem && doc.state == DocumentState::Complete
                    })
                    .map(|doc| doc.id.clone());

                if let Some(id) = matching_doc {
                    let from = "complete".to_string();
                    if let Some(doc) = self.state.get_document_mut(&id) {
                        doc.transition_to(DocumentState::Refining);
                        doc.refine_count += 1;
                    }
                    self.state.save()?;
                    self.emit(KanbanEvent::StateChanged {
                        id: id.clone(),
                        from,
                        to: "refining".to_string(),
                    });
                }
            }
        }

        Ok(())
    }

    /// Resolve the result directory for a document.
    ///
    /// When a document has a `project_id`, all tasks sharing that project write
    /// into a single `RESULT/<project_id>/` directory. Per-task session logs go
    /// under `.sessions/<file_stem>/` inside that directory.
    ///
    /// Without a `project_id`, each document gets its own result directory
    /// (or inherits from `continues_from`).
    fn result_dir_for(&self, doc: &KanbanDocument) -> PathBuf {
        if let Some(ref project_id) = doc.project_id {
            self.state.result_path().join(project_id)
        } else if let Some(ref parent_id) = doc.continues_from {
            if let Some(parent_doc) = self.state.get_document(parent_id) {
                self.state.result_path().join(parent_doc.file_stem())
            } else {
                self.state.result_path().join(doc.file_stem())
            }
        } else {
            self.state.result_path().join(doc.file_stem())
        }
    }

    /// Resolve the working directory for a document.
    /// If `work_dir` is set, the agent works in that directory (the session
    /// still goes in RESULT/ so work products target the external codebase).
    fn working_dir_for(&self, doc: &KanbanDocument) -> PathBuf {
        if let Some(ref wd) = doc.work_dir {
            wd.clone()
        } else {
            self.result_dir_for(doc)
        }
    }

    /// Resolve the session directory for a document.
    ///
    /// For project-based documents, sessions live in `.sessions/<file_stem>/`
    /// inside the shared result directory. For standalone documents, the session
    /// lives directly in the result directory.
    fn session_dir_for(&self, doc: &KanbanDocument) -> PathBuf {
        let result_dir = self.result_dir_for(doc);
        if doc.project_id.is_some() {
            result_dir.join(".sessions").join(doc.file_stem())
        } else {
            result_dir
        }
    }

    /// Process any documents that are ready for work.
    async fn dispatch_pending_work(&mut self) -> Result<(), MuAgentError> {
        // Process TODO documents whose dependencies are all satisfied
        let todo_docs: Vec<String> = self
            .state
            .documents_in_state(&DocumentState::Todo)
            .into_iter()
            .filter(|doc| {
                doc.depends_on.iter().all(|dep_task_id| {
                    self.state.documents.values().any(|d| {
                        d.task_id.as_deref() == Some(dep_task_id.as_str())
                            && d.state == DocumentState::Complete
                    })
                })
            })
            .map(|doc| doc.id.clone())
            .collect();

        if todo_docs.len() <= 1 {
            for doc_id in todo_docs {
                self.process_document_sequential(&doc_id).await?;
            }
        } else {
            self.process_documents_parallel(todo_docs).await?;
        }

        // Process REFINING documents
        let refining_docs: Vec<String> = self
            .state
            .documents_in_state(&DocumentState::Refining)
            .into_iter()
            .map(|doc| doc.id.clone())
            .collect();

        for doc_id in refining_docs {
            self.process_refinement(&doc_id).await?;
        }

        // Resume PROCESSING documents that came back from FEEDBACK
        let feedback_resumed: Vec<String> = self
            .state
            .documents_in_state(&DocumentState::Processing)
            .into_iter()
            .filter(|doc| {
                // Check if there's a response file indicating feedback was provided
                let response_path = self.state.processing_path().join(format!(
                    "{}_response.md",
                    doc.file_stem()
                ));
                response_path.exists()
            })
            .map(|doc| doc.id.clone())
            .collect();

        for doc_id in feedback_resumed {
            self.resume_from_feedback(&doc_id).await?;
        }

        Ok(())
    }

    /// Prepare a document for agent processing: move files, transition state,
    /// build config. Returns `None` if the document is missing.
    fn prepare_document(&mut self, doc_id: &str) -> Result<Option<TaskPrep>, MuAgentError> {
        let doc = match self.state.get_document(doc_id) {
            Some(doc) => doc.clone(),
            None => return Ok(None),
        };

        // Move from TODO/ to PROCESSING/
        let todo_file = self
            .state
            .todo_path()
            .join(format!("{}.md", doc.file_stem()));
        let processing_file = self
            .state
            .processing_path()
            .join(format!("{}.md", doc.file_stem()));

        if todo_file.exists() {
            fs_rename(&todo_file, &processing_file)?;
        }

        if let Some(d) = self.state.get_document_mut(doc_id) {
            d.transition_to(DocumentState::Processing);
        }
        self.state.save()?;

        self.emit(KanbanEvent::ProcessingStarted {
            id: doc_id.to_string(),
        });

        // Read document content, stripping any frontmatter preamble
        let content = if processing_file.exists() {
            let raw_content = std::fs::read_to_string(&processing_file)?;
            let (_preamble, body) = parse_preamble(&raw_content);
            body.to_string()
        } else {
            return Err(MuAgentError::InvalidState(format!(
                "document file missing: {}",
                processing_file.display()
            )));
        };

        // Resolve result directory, working directory, and session directory
        let result_dir = self.result_dir_for(&doc);
        let working_dir = self.working_dir_for(&doc);
        let session_dir = self.session_dir_for(&doc);
        fs_mkdir(&result_dir)?;
        fs_mkdir(&working_dir)?;
        fs_mkdir(&session_dir)?;

        let session_path = session_dir.join("session.jsonl");
        let config = AgentConfig {
            system_prompt: KANBAN_SYSTEM_PROMPT.to_string(),
            tools: crate::tools::kanban_tools(&working_dir, &self.state.root),
            working_directory: working_dir,
            session_store: SessionStore::from_path(session_path),
            ..self.config_template.clone()
        };

        let has_prior_work = doc.continues_from.is_some()
            || (doc.project_id.is_some() && !doc.depends_on.is_empty());
        let prompt = if has_prior_work {
            format!(
                "This is part of a multi-task project. The working directory may already contain \
                 files from earlier tasks. Build on the existing code according to the \
                 instructions below.\n\n{content}"
            )
        } else {
            content
        };

        Ok(Some(TaskPrep {
            doc_id: doc_id.to_string(),
            original_name: doc.original_name.clone(),
            processing_file,
            result_dir,
            session_dir,
            prompt,
            config,
        }))
    }

    /// Apply a completed task outcome back to the kanban state.
    fn apply_task_outcome(&mut self, outcome: TaskOutcome) -> Result<(), MuAgentError> {
        match outcome {
            TaskOutcome::Complete {
                doc_id,
                original_name,
                processing_file,
                session_dir,
            } => {
                if processing_file.exists() {
                    std::fs::remove_file(&processing_file)?;
                }
                generate_summary(&session_dir)?;
                if let Some(doc) = self.state.get_document_mut(&doc_id) {
                    doc.transition_to(DocumentState::Complete);
                }
                self.state.save()?;
                self.emit(KanbanEvent::ProcessingComplete {
                    id: doc_id.clone(),
                });
                self.stats
                    .log_activity(format!("completed: {original_name}"));
            }
            TaskOutcome::FeedbackRequested {
                doc_id,
                processing_file,
                question,
            } => {
                self.move_to_feedback(&doc_id, &processing_file, &question)?;
            }
            TaskOutcome::Error {
                doc_id,
                processing_file,
                error,
            } => {
                // Move the file back to TODO/ so it can be retried instead of
                // deleting it (which made retry impossible).
                if processing_file.exists() {
                    let todo_dest = self
                        .state
                        .todo_path()
                        .join(
                            processing_file
                                .file_name()
                                .unwrap_or_default(),
                        );
                    let _ = fs_rename(&processing_file, &todo_dest);
                }
                if let Some(doc) = self.state.get_document_mut(&doc_id) {
                    doc.error = Some(error.clone());
                    doc.transition_to(DocumentState::Error);
                }
                self.state.save()?;
                self.emit(KanbanEvent::Error {
                    id: Some(doc_id),
                    message: error,
                });
            }
        }
        Ok(())
    }

    /// Process multiple independent TODO documents in parallel.
    async fn process_documents_parallel(
        &mut self,
        doc_ids: Vec<String>,
    ) -> Result<(), MuAgentError> {
        // Phase 1: prepare all tasks sequentially (cheap fs ops + state transitions)
        let mut preparations = Vec::new();
        for doc_id in &doc_ids {
            if let Some(prep) = self.prepare_document(doc_id)? {
                preparations.push(prep);
            }
        }

        // Phase 2: spawn all agent tasks concurrently
        let handles: Vec<_> = preparations
            .into_iter()
            .map(|prep| tokio::spawn(run_agent_task(prep)))
            .collect();

        // Phase 3: collect results and apply outcomes sequentially
        let results = join_all(handles).await;
        for result in results {
            match result {
                Ok(outcome) => self.apply_task_outcome(outcome)?,
                Err(join_err) => {
                    // Task panicked — log but continue with other tasks
                    self.emit(KanbanEvent::Error {
                        id: None,
                        message: format!("task panicked: {join_err}"),
                    });
                }
            }
        }

        Ok(())
    }

    /// Process a single document sequentially (used when only one task is ready).
    async fn process_document_sequential(&mut self, doc_id: &str) -> Result<(), MuAgentError> {
        let prep = match self.prepare_document(doc_id)? {
            Some(prep) => prep,
            None => return Ok(()),
        };
        let outcome = run_agent_task(prep).await;
        self.apply_task_outcome(outcome)
    }

    async fn process_refinement(&mut self, doc_id: &str) -> Result<(), MuAgentError> {
        let doc = match self.state.get_document(doc_id) {
            Some(doc) => doc.clone(),
            None => return Ok(()),
        };

        // Transition to Processing
        if let Some(doc) = self.state.get_document_mut(doc_id) {
            doc.transition_to(DocumentState::Processing);
        }
        self.state.save()?;

        self.emit(KanbanEvent::ProcessingStarted {
            id: doc_id.to_string(),
        });

        // Read the comments file
        let comments_path = self
            .state
            .refine_path()
            .join(format!("{}_COMMENTS.md", doc.file_stem()));
        let comments = if comments_path.exists() {
            std::fs::read_to_string(&comments_path)?
        } else {
            return Err(MuAgentError::InvalidState(format!(
                "comments file missing: {}",
                comments_path.display()
            )));
        };

        // Resume session in the existing result directory
        let result_dir = self.result_dir_for(&doc);
        let session_dir = self.session_dir_for(&doc);
        let session_path = session_dir.join("session.jsonl");

        let config = AgentConfig {
            system_prompt: KANBAN_SYSTEM_PROMPT.to_string(),
            tools: crate::tools::kanban_tools(&result_dir, &self.state.root),
            working_directory: result_dir.clone(),
            session_store: SessionStore::from_path(session_path),
            ..self.config_template.clone()
        };

        let agent = Agent::new(config).await?;
        let refinement_prompt = format!(
            "The user has requested refinements to your previous work. Here are their comments:\n\n{comments}"
        );
        let result = agent.prompt(refinement_prompt).await;

        // Clean up comments file
        if comments_path.exists() {
            std::fs::remove_file(&comments_path)?;
        }

        match result {
            Ok(_message) => {
                generate_summary(&session_dir)?;
                if let Some(doc) = self.state.get_document_mut(doc_id) {
                    doc.transition_to(DocumentState::Complete);
                }
                self.state.save()?;
                self.emit(KanbanEvent::ProcessingComplete {
                    id: doc_id.to_string(),
                });
                self.stats
                    .log_activity(format!("refined: {}", doc.original_name));
            }
            Err(err) => {
                if let Some(doc) = self.state.get_document_mut(doc_id) {
                    doc.error = Some(err.to_string());
                    doc.transition_to(DocumentState::Error);
                }
                self.state.save()?;
                self.emit(KanbanEvent::Error {
                    id: Some(doc_id.to_string()),
                    message: err.to_string(),
                });
            }
        }

        Ok(())
    }

    async fn resume_from_feedback(&mut self, doc_id: &str) -> Result<(), MuAgentError> {
        let doc = match self.state.get_document(doc_id) {
            Some(doc) => doc.clone(),
            None => return Ok(()),
        };

        self.emit(KanbanEvent::ProcessingStarted {
            id: doc_id.to_string(),
        });

        // Read the response file
        let response_path = self
            .state
            .processing_path()
            .join(format!("{}_response.md", doc.file_stem()));
        let response = if response_path.exists() {
            std::fs::read_to_string(&response_path)?
        } else {
            return Ok(()); // No response yet
        };

        // Resume session
        let result_dir = self.result_dir_for(&doc);
        let session_dir = self.session_dir_for(&doc);
        let session_path = session_dir.join("session.jsonl");

        let config = AgentConfig {
            system_prompt: KANBAN_SYSTEM_PROMPT.to_string(),
            tools: crate::tools::kanban_tools(&result_dir, &self.state.root),
            working_directory: result_dir.clone(),
            session_store: SessionStore::from_path(session_path),
            ..self.config_template.clone()
        };

        let agent = Agent::new(config).await?;
        let feedback_prompt =
            format!("The user has responded to your feedback request:\n\n{response}");
        let result = agent.prompt(feedback_prompt).await;

        // Clean up response file and processing file
        if response_path.exists() {
            std::fs::remove_file(&response_path)?;
        }
        let processing_file = self
            .state
            .processing_path()
            .join(format!("{}.md", doc.file_stem()));
        if processing_file.exists() {
            std::fs::remove_file(&processing_file)?;
        }

        // Clean up feedback_request.md from result dir
        let feedback_request = result_dir.join("feedback_request.md");
        if feedback_request.exists() {
            std::fs::remove_file(&feedback_request)?;
        }

        match result {
            Ok(_message) => {
                // Check if agent requested feedback again
                let feedback_request = result_dir.join("feedback_request.md");
                if feedback_request.exists() {
                    let question = std::fs::read_to_string(&feedback_request)
                        .unwrap_or_else(|_| "Feedback requested".to_string());
                    // Re-create processing file for the feedback flow
                    let doc_source = session_dir.join("original_task.md");
                    if doc_source.exists() {
                        let content = std::fs::read_to_string(&doc_source)?;
                        fs_write(&processing_file, &content)?;
                    }
                    self.move_to_feedback(doc_id, &processing_file, &question)?;
                } else {
                    generate_summary(&session_dir)?;
                    if let Some(doc) = self.state.get_document_mut(doc_id) {
                        doc.transition_to(DocumentState::Complete);
                    }
                    self.state.save()?;
                    self.emit(KanbanEvent::ProcessingComplete {
                        id: doc_id.to_string(),
                    });
                }
            }
            Err(err) => {
                if let Some(doc) = self.state.get_document_mut(doc_id) {
                    doc.error = Some(err.to_string());
                    doc.transition_to(DocumentState::Error);
                }
                self.state.save()?;
                self.emit(KanbanEvent::Error {
                    id: Some(doc_id.to_string()),
                    message: err.to_string(),
                });
            }
        }

        Ok(())
    }

    fn move_to_feedback(
        &mut self,
        doc_id: &str,
        processing_file: &Path,
        question: &str,
    ) -> Result<(), MuAgentError> {
        let doc = match self.state.get_document(doc_id) {
            Some(doc) => doc.clone(),
            None => return Ok(()),
        };

        let feedback_file = self
            .state
            .feedback_path()
            .join(format!("{}.md", doc.file_stem()));

        if processing_file.exists() {
            fs_rename(processing_file, &feedback_file)?;
        }

        // Write the feedback request alongside
        let feedback_request_dest = self
            .state
            .feedback_path()
            .join(format!("{}_question.md", doc.file_stem()));
        fs_write(&feedback_request_dest, question)?;

        if let Some(doc) = self.state.get_document_mut(doc_id) {
            doc.transition_to(DocumentState::Feedback);
        }
        self.state.save()?;

        self.emit(KanbanEvent::FeedbackRequested {
            id: doc_id.to_string(),
            question: question.to_string(),
        });
        self.stats.log_activity(format!(
            "feedback requested: {}",
            doc.original_name
        ));

        Ok(())
    }

    fn update_stats(&mut self) -> Result<(), MuAgentError> {
        self.stats = KanbanStats::from_state(&self.state);
        // Preserve recent activity across recalculations
        self.stats.write_stats_file(&self.state)?;
        self.emit(KanbanEvent::StatsUpdated(self.stats.clone()));
        Ok(())
    }
}

/// Generate a human-readable `SUMMARY.md` from a `session.jsonl` file.
fn generate_summary(result_dir: &Path) -> Result<(), MuAgentError> {
    let session_path = result_dir.join("session.jsonl");
    if !session_path.exists() {
        return Ok(());
    }

    let store = SessionStore::from_path(session_path);
    let entries = store.load_entries()?;
    if entries.is_empty() {
        return Ok(());
    }

    let mut md = String::new();
    md.push_str("# Session Summary\n\n");

    // Extract the initial task from the first user message
    if let Some(task_entry) = entries.iter().find(|e| e.message.role == Role::User) {
        let task_text = task_entry.message.plain_text();
        if !task_text.is_empty() {
            md.push_str("## Task\n\n");
            md.push_str(&task_text);
            md.push_str("\n\n");
        }
    }

    // Collect tool usage and assistant messages
    let mut tools_used: Vec<String> = Vec::new();
    let mut assistant_messages: Vec<&SessionEntry> = Vec::new();

    for entry in &entries {
        if entry.message.role == Role::Assistant {
            for part in &entry.message.content {
                if let ContentPart::ToolCall(tc) = part {
                    if !tools_used.contains(&tc.name) {
                        tools_used.push(tc.name.clone());
                    }
                }
            }
            assistant_messages.push(entry);
        }
    }

    // Tools used
    if !tools_used.is_empty() {
        md.push_str("## Tools Used\n\n");
        for tool in &tools_used {
            md.push_str(&format!("- `{tool}`\n"));
        }
        md.push('\n');
    }

    // Conversation timeline
    md.push_str("## Conversation\n\n");
    for entry in &entries {
        let role_label = match entry.message.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool",
            Role::System => continue,
        };

        let text = summarize_entry(entry);
        if text.is_empty() {
            continue;
        }

        md.push_str(&format!("**{role_label}**: {text}\n\n"));
    }

    // Final result: extract the last assistant text response
    if let Some(last) = assistant_messages.last() {
        let final_text = last.message.plain_text();
        if !final_text.is_empty() {
            md.push_str("## Result\n\n");
            md.push_str(&final_text);
            md.push('\n');
        }
    }

    let summary_path = result_dir.join("SUMMARY.md");
    fs_write(&summary_path, &md)?;
    Ok(())
}

/// Produce a concise one-line summary of a session entry.
fn summarize_entry(entry: &SessionEntry) -> String {
    match entry.message.role {
        Role::User | Role::System => entry.message.plain_text(),
        Role::Assistant => {
            let mut parts: Vec<String> = Vec::new();
            for part in &entry.message.content {
                match part {
                    ContentPart::Text { text } => {
                        if !text.is_empty() {
                            parts.push(truncate(text, 200));
                        }
                    }
                    ContentPart::ToolCall(tc) => {
                        parts.push(format!("`{}(..)`", tc.name));
                    }
                    _ => {}
                }
            }
            parts.join(" ")
        }
        Role::Tool => {
            for part in &entry.message.content {
                if let ContentPart::ToolResult {
                    tool_name, result, ..
                } = part
                {
                    return format!("`{}` → {}", tool_name, truncate(result, 120));
                }
            }
            String::new()
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    // Truncate to first line or max chars, whichever is shorter
    let first_line = s.lines().next().unwrap_or(s);
    if first_line.len() <= max {
        first_line.to_string()
    } else {
        format!("{}…", &first_line[..max])
    }
}

#[cfg(test)]
mod tests {
    use mu_ai::{Message, ToolCall};
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn generate_summary_creates_markdown() {
        let tempdir = TempDir::new().unwrap();
        let result_dir = tempdir.path().join("result");
        std::fs::create_dir_all(&result_dir).unwrap();

        let store = SessionStore::from_path(result_dir.join("session.jsonl"));
        store
            .append(None, &Message::text(Role::User, "Write a hello world program"))
            .unwrap();
        let entry = store
            .append(
                None,
                &Message {
                    role: Role::Assistant,
                    content: vec![
                        ContentPart::Text {
                            text: "I'll create the file.".to_string(),
                        },
                        ContentPart::ToolCall(ToolCall {
                            id: "tc1".to_string(),
                            name: "write".to_string(),
                            arguments: json!({"path": "hello.py"}),
                        }),
                    ],
                    name: None,
                    tool_call_id: None,
                },
            )
            .unwrap();
        store
            .append(
                Some(entry.id),
                &Message::with_tool_result("tc1", "write", "ok", false),
            )
            .unwrap();
        store
            .append(
                None,
                &Message::text(Role::Assistant, "Done! Created hello.py."),
            )
            .unwrap();

        generate_summary(&result_dir).unwrap();

        let summary = std::fs::read_to_string(result_dir.join("SUMMARY.md")).unwrap();
        assert!(summary.contains("# Session Summary"));
        assert!(summary.contains("## Task"));
        assert!(summary.contains("Write a hello world program"));
        assert!(summary.contains("## Tools Used"));
        assert!(summary.contains("- `write`"));
        assert!(summary.contains("## Conversation"));
        assert!(summary.contains("## Result"));
        assert!(summary.contains("Done! Created hello.py."));
    }

    #[test]
    fn generate_summary_skips_missing_session() {
        let tempdir = TempDir::new().unwrap();
        // No session.jsonl exists — should return Ok without creating SUMMARY.md
        generate_summary(tempdir.path()).unwrap();
        assert!(!tempdir.path().join("SUMMARY.md").exists());
    }

    fn make_doc_with_task_id(id: &str, task_id: Option<&str>, state: DocumentState) -> KanbanDocument {
        let mut doc = KanbanDocument::new(id.to_string(), "test".to_string());
        doc.task_id = task_id.map(|s| s.to_string());
        doc.state = state;
        doc
    }

    #[test]
    fn depends_on_blocks_dispatch_when_unmet() {
        // Task C depends on A, but A is not complete → C should be filtered out
        let mut doc_c = KanbanDocument::new("c-id".to_string(), "task-c".to_string());
        doc_c.depends_on = vec!["task-a".to_string()];

        let doc_a = make_doc_with_task_id("a-id", Some("task-a"), DocumentState::Todo);

        // Simulate the filter logic from dispatch_pending_work
        let documents: Vec<&KanbanDocument> = vec![&doc_a, &doc_c];
        let passes = doc_c.depends_on.iter().all(|dep_task_id| {
            documents.iter().any(|d| {
                d.task_id.as_deref() == Some(dep_task_id.as_str())
                    && d.state == DocumentState::Complete
            })
        });
        assert!(!passes, "task C should be blocked because task A is not complete");
    }

    #[test]
    fn depends_on_allows_when_all_deps_complete() {
        let mut doc_c = KanbanDocument::new("c-id".to_string(), "task-c".to_string());
        doc_c.depends_on = vec!["task-a".to_string(), "task-b".to_string()];

        let doc_a = make_doc_with_task_id("a-id", Some("task-a"), DocumentState::Complete);
        let doc_b = make_doc_with_task_id("b-id", Some("task-b"), DocumentState::Complete);

        let documents: Vec<&KanbanDocument> = vec![&doc_a, &doc_b, &doc_c];
        let passes = doc_c.depends_on.iter().all(|dep_task_id| {
            documents.iter().any(|d| {
                d.task_id.as_deref() == Some(dep_task_id.as_str())
                    && d.state == DocumentState::Complete
            })
        });
        assert!(passes, "task C should pass because both A and B are complete");
    }

    #[test]
    fn depends_on_partial_blocks() {
        // One dep complete, one not → still blocked
        let mut doc_c = KanbanDocument::new("c-id".to_string(), "task-c".to_string());
        doc_c.depends_on = vec!["task-a".to_string(), "task-b".to_string()];

        let doc_a = make_doc_with_task_id("a-id", Some("task-a"), DocumentState::Complete);
        let doc_b = make_doc_with_task_id("b-id", Some("task-b"), DocumentState::Processing);

        let documents: Vec<&KanbanDocument> = vec![&doc_a, &doc_b, &doc_c];
        let passes = doc_c.depends_on.iter().all(|dep_task_id| {
            documents.iter().any(|d| {
                d.task_id.as_deref() == Some(dep_task_id.as_str())
                    && d.state == DocumentState::Complete
            })
        });
        assert!(!passes, "task C should be blocked because task B is not complete");
    }

    #[test]
    fn project_id_resolves_continues_from() {
        use std::collections::HashMap;

        let parent = make_doc_with_task_id("parent-uuid", Some("setup"), DocumentState::Complete);
        let mut documents = HashMap::new();
        documents.insert(parent.id.clone(), parent);

        // Simulate the project_id → continues_from resolution from scan_and_reconcile
        let project_id = "parent-uuid";
        let continues_from = documents
            .values()
            .find(|d| d.id == project_id)
            .map(|d| d.id.clone());

        assert_eq!(continues_from, Some("parent-uuid".to_string()));
    }

    // --- Parallelization tests ---

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;
    use mu_ai::{
        AssistantEvent, AssistantEventStream, ChatProvider, ModelSpec, ProviderId,
        StopReason, StreamRequest,
    };

    /// A provider that tracks peak concurrency across calls.
    struct ConcurrencyTrackingProvider {
        active: AtomicUsize,
        peak: AtomicUsize,
        delay: Duration,
    }

    impl ConcurrencyTrackingProvider {
        fn new(delay: Duration) -> Self {
            Self {
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                delay,
            }
        }

        fn peak(&self) -> usize {
            self.peak.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ChatProvider for ConcurrencyTrackingProvider {
        async fn stream(
            &self,
            _request: StreamRequest,
        ) -> Result<AssistantEventStream, mu_ai::MuAiError> {
            let current = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(current, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.active.fetch_sub(1, Ordering::SeqCst);

            Ok(Box::pin(futures::stream::iter(vec![
                Ok(AssistantEvent::TextDelta {
                    delta: "Done.".to_string(),
                }),
                Ok(AssistantEvent::Stop {
                    reason: StopReason::EndTurn,
                }),
            ])))
        }
    }

    /// A simple provider that returns "Done." immediately (no delay).
    struct InstantProvider;

    #[async_trait]
    impl ChatProvider for InstantProvider {
        async fn stream(
            &self,
            _request: StreamRequest,
        ) -> Result<AssistantEventStream, mu_ai::MuAiError> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(AssistantEvent::TextDelta {
                    delta: "Done.".to_string(),
                }),
                Ok(AssistantEvent::Stop {
                    reason: StopReason::EndTurn,
                }),
            ])))
        }
    }

    fn test_model() -> ModelSpec {
        ModelSpec::new(
            ProviderId::OpenAiCompatible,
            "test-model",
            "Test Model",
            128_000,
            16_384,
        )
    }

    fn test_config(provider: Arc<dyn ChatProvider>) -> AgentConfig {
        AgentConfig {
            system_prompt: String::new(),
            model: test_model(),
            provider,
            tools: Vec::new(),
            working_directory: PathBuf::new(),
            session_store: SessionStore::from_path(PathBuf::new()),
            max_turns: 4,
            auto_compact_threshold: 100,
        }
    }

    /// Set up a kanban board directory with task files in TODO/.
    fn setup_board(
        tempdir: &TempDir,
        tasks: &[(&str, &str)], // (filename, content)
    ) {
        let todo = tempdir.path().join("TODO");
        std::fs::create_dir_all(&todo).expect("create TODO");
        for &(name, content) in tasks {
            std::fs::write(todo.join(name), content).expect("write task file");
        }
    }

    fn make_runner(
        root: PathBuf,
        provider: Arc<dyn ChatProvider>,
    ) -> (KanbanRunner, broadcast::Receiver<KanbanEvent>) {
        let (runner, event_rx, _event_tx, _command_tx) =
            KanbanRunner::new(root, test_config(provider)).expect("create runner");
        (runner, event_rx)
    }

    #[tokio::test]
    async fn parallel_dispatch_runs_independent_tasks_concurrently() {
        let tempdir = TempDir::new().expect("tempdir");
        setup_board(
            &tempdir,
            &[
                ("task-a.md", "Task A"),
                ("task-b.md", "Task B"),
                ("task-c.md", "Task C"),
            ],
        );

        let provider = Arc::new(ConcurrencyTrackingProvider::new(Duration::from_millis(50)));
        let provider_ref = Arc::clone(&provider);
        let (mut runner, _rx) = make_runner(tempdir.path().to_path_buf(), provider);

        runner.scan_and_reconcile().expect("scan");
        assert_eq!(
            runner.state.documents_in_state(&DocumentState::Todo).len(),
            3,
            "all 3 tasks should be discovered"
        );

        runner.dispatch_pending_work().await.expect("dispatch");

        // All 3 independent tasks should have been spawned concurrently
        assert!(
            provider_ref.peak() >= 2,
            "expected peak concurrency >= 2, got {}",
            provider_ref.peak()
        );

        // All should be complete
        assert_eq!(
            runner
                .state
                .documents_in_state(&DocumentState::Complete)
                .len(),
            3,
            "all 3 tasks should be complete"
        );
    }

    #[tokio::test]
    async fn dependent_tasks_wait_for_predecessors() {
        let tempdir = TempDir::new().expect("tempdir");
        setup_board(
            &tempdir,
            &[
                (
                    "task-a.md",
                    "---\ntask_id: task-a\n---\nTask A: independent",
                ),
                (
                    "task-b.md",
                    "---\ntask_id: task-b\n---\nTask B: independent",
                ),
                (
                    "task-c.md",
                    "---\ntask_id: task-c\ndepends_on: task-a, task-b\n---\nTask C: depends on A and B",
                ),
            ],
        );

        let provider: Arc<dyn ChatProvider> = Arc::new(InstantProvider);
        let (mut runner, _rx) = make_runner(tempdir.path().to_path_buf(), provider.clone());

        runner.scan_and_reconcile().expect("scan");
        assert_eq!(runner.state.documents.len(), 3);

        // First dispatch: only A and B should be processed (C is blocked)
        runner.dispatch_pending_work().await.expect("dispatch 1");

        let complete = runner.state.documents_in_state(&DocumentState::Complete);
        let todo = runner.state.documents_in_state(&DocumentState::Todo);
        assert_eq!(complete.len(), 2, "A and B should be complete");
        assert_eq!(todo.len(), 1, "C should still be in todo");
        assert_eq!(
            todo[0].task_id.as_deref(),
            Some("task-c"),
            "the remaining todo task should be C"
        );

        // Second dispatch: C's deps are now met
        runner.dispatch_pending_work().await.expect("dispatch 2");

        let complete = runner.state.documents_in_state(&DocumentState::Complete);
        assert_eq!(complete.len(), 3, "all 3 tasks should now be complete");
    }

    #[tokio::test]
    async fn single_task_uses_sequential_path() {
        // When only 1 task is ready, it should go through the sequential path
        // (no tokio::spawn overhead). Verify it still completes.
        let tempdir = TempDir::new().expect("tempdir");
        setup_board(&tempdir, &[("only-task.md", "The only task")]);

        let provider: Arc<dyn ChatProvider> = Arc::new(InstantProvider);
        let (mut runner, _rx) = make_runner(tempdir.path().to_path_buf(), provider);

        runner.scan_and_reconcile().expect("scan");
        runner.dispatch_pending_work().await.expect("dispatch");

        assert_eq!(
            runner
                .state
                .documents_in_state(&DocumentState::Complete)
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn parallel_dispatch_emits_events_for_all_tasks() {
        let tempdir = TempDir::new().expect("tempdir");
        setup_board(
            &tempdir,
            &[("t1.md", "Task 1"), ("t2.md", "Task 2")],
        );

        let provider: Arc<dyn ChatProvider> = Arc::new(InstantProvider);
        let (mut runner, mut rx) = make_runner(tempdir.path().to_path_buf(), provider);

        runner.scan_and_reconcile().expect("scan");
        runner.dispatch_pending_work().await.expect("dispatch");

        // Drain events and count ProcessingStarted + ProcessingComplete
        let mut started = 0;
        let mut completed = 0;
        while let Ok(event) = rx.try_recv() {
            match event {
                KanbanEvent::DocumentDiscovered { .. } => {}
                KanbanEvent::ProcessingStarted { .. } => started += 1,
                KanbanEvent::ProcessingComplete { .. } => completed += 1,
                _ => {}
            }
        }
        assert_eq!(started, 2, "should emit ProcessingStarted for both tasks");
        assert_eq!(completed, 2, "should emit ProcessingComplete for both tasks");
    }

    #[tokio::test]
    async fn diamond_dependency_dag() {
        // Diamond: A → B, A → C, B+C → D
        let tempdir = TempDir::new().expect("tempdir");
        setup_board(
            &tempdir,
            &[
                ("a.md", "---\ntask_id: a\n---\nTask A"),
                ("b.md", "---\ntask_id: b\ndepends_on: a\n---\nTask B"),
                ("c.md", "---\ntask_id: c\ndepends_on: a\n---\nTask C"),
                (
                    "d.md",
                    "---\ntask_id: d\ndepends_on: b, c\n---\nTask D",
                ),
            ],
        );

        let provider: Arc<dyn ChatProvider> = Arc::new(InstantProvider);
        let (mut runner, _rx) = make_runner(tempdir.path().to_path_buf(), provider.clone());

        runner.scan_and_reconcile().expect("scan");

        // Round 1: only A is eligible (B, C, D all have unmet deps)
        runner.dispatch_pending_work().await.expect("round 1");
        assert_eq!(
            runner
                .state
                .documents_in_state(&DocumentState::Complete)
                .len(),
            1,
            "only A should be complete"
        );

        // Round 2: B and C are now eligible (both depend only on A)
        runner.dispatch_pending_work().await.expect("round 2");
        assert_eq!(
            runner
                .state
                .documents_in_state(&DocumentState::Complete)
                .len(),
            3,
            "A, B, and C should be complete"
        );

        // Round 3: D is now eligible
        runner.dispatch_pending_work().await.expect("round 3");
        assert_eq!(
            runner
                .state
                .documents_in_state(&DocumentState::Complete)
                .len(),
            4,
            "all 4 tasks should be complete"
        );
    }

    #[tokio::test]
    async fn fan_out_fan_in_a_spawns_bcde() {
        // Fan-out/fan-in: A → B, C, D (parallel) → E
        let tempdir = TempDir::new().expect("tempdir");
        setup_board(
            &tempdir,
            &[
                ("a.md", "---\ntask_id: a\n---\nTask A"),
                ("b.md", "---\ntask_id: b\ndepends_on: a\n---\nTask B"),
                ("c.md", "---\ntask_id: c\ndepends_on: a\n---\nTask C"),
                ("d.md", "---\ntask_id: d\ndepends_on: a\n---\nTask D"),
                (
                    "e.md",
                    "---\ntask_id: e\ndepends_on: b, c, d\n---\nTask E",
                ),
            ],
        );

        let provider = Arc::new(ConcurrencyTrackingProvider::new(Duration::from_millis(50)));
        let provider_ref = Arc::clone(&provider);
        let (mut runner, _rx) = make_runner(tempdir.path().to_path_buf(), provider);

        runner.scan_and_reconcile().expect("scan");

        // Round 1: only A is eligible
        runner.dispatch_pending_work().await.expect("round 1");
        let complete = runner.state.documents_in_state(&DocumentState::Complete);
        let todo = runner.state.documents_in_state(&DocumentState::Todo);
        assert_eq!(complete.len(), 1, "only A should be complete");
        assert_eq!(
            complete[0].task_id.as_deref(),
            Some("a"),
            "completed task should be A"
        );
        assert_eq!(todo.len(), 4, "B, C, D, E should still be in todo");

        // Round 2: B, C, D are now eligible (all depend only on A); E still blocked
        runner.dispatch_pending_work().await.expect("round 2");
        let complete = runner.state.documents_in_state(&DocumentState::Complete);
        let todo = runner.state.documents_in_state(&DocumentState::Todo);
        assert_eq!(complete.len(), 4, "A, B, C, D should be complete");
        assert_eq!(todo.len(), 1, "only E should remain in todo");
        assert_eq!(
            todo[0].task_id.as_deref(),
            Some("e"),
            "the remaining todo task should be E"
        );

        // Round 3: E is now eligible
        runner.dispatch_pending_work().await.expect("round 3");
        let complete = runner.state.documents_in_state(&DocumentState::Complete);
        assert_eq!(complete.len(), 5, "all 5 tasks should be complete");

        // B, C, D ran in parallel so peak concurrency must be >= 3
        assert!(
            provider_ref.peak() >= 3,
            "peak concurrency should be >= 3, was {}",
            provider_ref.peak()
        );
    }
}
