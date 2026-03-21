use async_trait::async_trait;

use crate::{AssistantEventStream, ChatProvider, MuAiError, StreamRequest};

mod anthropic;
mod openai;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiCompatibleProvider;

#[derive(Clone, Default)]
pub struct RouterProvider {
    openai: OpenAiCompatibleProvider,
    anthropic: AnthropicProvider,
}

#[async_trait]
impl ChatProvider for RouterProvider {
    async fn stream(&self, request: StreamRequest) -> Result<AssistantEventStream, MuAiError> {
        match request.model.provider {
            crate::ProviderId::OpenAiCompatible => self.openai.stream(request).await,
            crate::ProviderId::Anthropic => self.anthropic.stream(request).await,
        }
    }
}
