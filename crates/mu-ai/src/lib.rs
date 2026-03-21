#![allow(missing_docs)]
#![allow(unused_crate_dependencies)]

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub mod models;
mod providers;
mod sse;

pub use models::{load_custom_models, ModelId, ModelRegistry, ModelSpec, ProviderId};
pub use providers::{AnthropicProvider, OpenAiCompatibleProvider, RouterProvider};
pub use sse::{stream_sse_frames, SseFrame};

pub type AssistantEventStream =
    Pin<Box<dyn Stream<Item = Result<AssistantEvent, MuAiError>> + Send>>;

#[derive(Debug, Error)]
pub enum MuAiError {
    #[error("missing environment variable {0}")]
    MissingEnvVar(&'static str),
    #[error("http error: {0}")]
    Http(String),
    #[error("provider error: {0}")]
    Provider(String),
    #[error("invalid SSE frame: {0}")]
    InvalidSseFrame(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("invalid tool arguments: {0}")]
    InvalidToolArguments(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Request(#[from] reqwest::Error),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            content: vec![ContentPart::Text { text: text.into() }],
            name: None,
            tool_call_id: None,
        }
    }

    pub fn plain_text(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ToolResult { result, .. } => Some(result.as_str()),
                ContentPart::ToolCall(_) => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn with_tool_result(
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        result: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: vec![ContentPart::ToolResult {
                tool_call_id: tool_call_id.into(),
                tool_name: tool_name.into(),
                result: result.into(),
                is_error,
            }],
            name: None,
            tool_call_id: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    ToolCall(ToolCall),
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        result: String,
        is_error: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StreamRequest {
    pub model: ModelSpec,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Usage {
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolCall,
    MaxTokens,
    Cancelled,
    Unknown(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantEvent {
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
    Usage {
        usage: Usage,
    },
    Stop {
        reason: StopReason,
    },
}

#[async_trait]
pub trait ChatProvider: Send + Sync {
    async fn stream(&self, request: StreamRequest) -> Result<AssistantEventStream, MuAiError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderCredentials {
    pub api_key: String,
    pub base_url: String,
}

pub fn load_provider_credentials(provider: &ProviderId) -> Result<ProviderCredentials, MuAiError> {
    match provider {
        ProviderId::OpenAiCompatible => {
            let api_key = std::env::var("MU_OPENAI_API_KEY")
                .ok()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok())
                .or_else(|| std::env::var("MU_API_KEY").ok())
                .ok_or(MuAiError::MissingEnvVar(
                    "MU_OPENAI_API_KEY or OPENAI_API_KEY",
                ))?;
            let base_url = std::env::var("MU_OPENAI_BASE_URL")
                .ok()
                .or_else(|| std::env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Ok(ProviderCredentials { api_key, base_url })
        }
        ProviderId::Anthropic => {
            let api_key = std::env::var("MU_ANTHROPIC_API_KEY")
                .ok()
                .or_else(|| std::env::var("ANTHROPIC_API_KEY").ok())
                .ok_or(MuAiError::MissingEnvVar(
                    "MU_ANTHROPIC_API_KEY or ANTHROPIC_API_KEY",
                ))?;
            let base_url = std::env::var("MU_ANTHROPIC_BASE_URL")
                .ok()
                .unwrap_or_else(|| "https://api.anthropic.com".to_string());
            Ok(ProviderCredentials { api_key, base_url })
        }
    }
}
