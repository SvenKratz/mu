use std::collections::{BTreeMap, BTreeSet};

use async_stream::try_stream;
use async_trait::async_trait;
use futures::TryStreamExt;
use reqwest::header::CONTENT_TYPE;
use serde_json::{json, Value};

use crate::models::ProviderId;
use crate::sse::stream_sse_frames;
use crate::{
    load_provider_credentials, AssistantEvent, AssistantEventStream, ChatProvider, ContentPart,
    Message, MuAiError, StopReason, StreamRequest, ToolCall, Usage,
};

#[derive(Clone)]
pub struct AnthropicProvider {
    client: reqwest::Client,
}

impl Default for AnthropicProvider {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl ChatProvider for AnthropicProvider {
    async fn stream(&self, request: StreamRequest) -> Result<AssistantEventStream, MuAiError> {
        let credentials = load_provider_credentials(&ProviderId::Anthropic)?;
        let (system, messages) = to_anthropic_messages(&request.messages);
        let body = json!({
            "model": request.model.id.0,
            "system": system,
            "messages": messages,
            "tools": request.tools.iter().map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.input_schema,
                })
            }).collect::<Vec<_>>(),
            "stream": true,
            "temperature": request.temperature,
            "max_tokens": request.max_tokens.unwrap_or(4096),
        });

        let endpoint = format!("{}/v1/messages", credentials.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(endpoint)
            .header("x-api-key", credentials.api_key)
            .header("anthropic-version", "2023-06-01")
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "failed to read response body".to_string());
            return Err(MuAiError::Http(format!("{status}: {body}")));
        }

        let frames = stream_sse_frames(response);
        let stream = try_stream! {
            let mut partial_calls: BTreeMap<u64, PartialToolCall> = BTreeMap::new();
            let mut emitted_calls = BTreeSet::new();

            futures::pin_mut!(frames);
            while let Some(frame) = frames.try_next().await? {
                let event_name = frame.event.unwrap_or_else(|| "message".to_string());
                let payload: Value = if frame.data.trim().is_empty() {
                    Value::Null
                } else {
                    serde_json::from_str(&frame.data)?
                };

                match event_name.as_str() {
                    "message_start" => {
                        if let Some(usage) = payload
                            .get("message")
                            .and_then(|message| message.get("usage"))
                            .and_then(parse_anthropic_usage)
                        {
                            yield AssistantEvent::Usage { usage };
                        }
                    }
                    "content_block_start" => {
                        if let Some(block) = payload.get("content_block") {
                            let is_tool = block.get("type").and_then(Value::as_str) == Some("tool_use");
                            if is_tool {
                                let index = payload
                                    .get("index")
                                    .and_then(Value::as_u64)
                                    .unwrap_or(0);
                                partial_calls.insert(
                                    index,
                                    PartialToolCall {
                                        id: block.get("id").and_then(Value::as_str).map(ToString::to_string),
                                        name: block.get("name").and_then(Value::as_str).map(ToString::to_string),
                                        arguments: String::new(),
                                    },
                                );
                            }
                        }
                    }
                    "content_block_delta" => {
                        let index = payload.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some(delta) = payload.get("delta") {
                            match delta.get("type").and_then(Value::as_str) {
                                Some("text_delta") => {
                                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                        yield AssistantEvent::TextDelta {
                                            delta: text.to_string(),
                                        };
                                    }
                                }
                                Some("input_json_delta") => {
                                    if let Some(fragment) = delta.get("partial_json").and_then(Value::as_str) {
                                        let entry = partial_calls.entry(index).or_default();
                                        entry.arguments.push_str(fragment);
                                        yield AssistantEvent::ToolCallDelta {
                                            id: entry
                                                .id
                                                .clone()
                                                .unwrap_or_else(|| format!("tool_{index}")),
                                            name: entry.name.clone(),
                                            partial_json: fragment.to_string(),
                                        };
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "content_block_stop" => {
                        let index = payload.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some(call) = partial_calls.get(&index) {
                            if !emitted_calls.contains(&index) {
                                emitted_calls.insert(index);
                                yield AssistantEvent::ToolCall {
                                    call: finalize_tool_call(index, call)?,
                                };
                            }
                        }
                    }
                    "message_delta" => {
                        if let Some(usage) = payload.get("usage").and_then(parse_anthropic_usage) {
                            yield AssistantEvent::Usage { usage };
                        }
                    }
                    "message_stop" => {
                        for (index, call) in &partial_calls {
                            if !emitted_calls.contains(index) {
                                emitted_calls.insert(*index);
                                yield AssistantEvent::ToolCall {
                                    call: finalize_tool_call(*index, call)?,
                                };
                            }
                        }
                        yield AssistantEvent::Stop {
                            reason: StopReason::EndTurn,
                        };
                    }
                    "error" => {
                        let message = payload
                            .get("error")
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown anthropic error");
                        Err(MuAiError::Provider(message.to_string()))?;
                    }
                    _ => {}
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

fn to_anthropic_messages(messages: &[Message]) -> (String, Vec<Value>) {
    let mut system_lines = Vec::new();
    let mut rendered = Vec::new();

    for message in messages {
        match message.role {
            crate::Role::System => system_lines.push(message.plain_text()),
            crate::Role::User => rendered.push(json!({
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": message.plain_text(),
                }],
            })),
            crate::Role::Assistant => {
                let mut content = Vec::new();
                for part in &message.content {
                    match part {
                        ContentPart::Text { text } => content.push(json!({
                            "type": "text",
                            "text": text,
                        })),
                        ContentPart::ToolCall(call) => content.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": call.arguments,
                        })),
                        ContentPart::ToolResult { .. } => {}
                    }
                }
                rendered.push(json!({
                    "role": "assistant",
                    "content": content,
                }));
            }
            crate::Role::Tool => {
                let mut content = Vec::new();
                for part in &message.content {
                    if let ContentPart::ToolResult {
                        tool_call_id,
                        tool_name,
                        result,
                        is_error,
                    } = part
                    {
                        content.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": result,
                            "is_error": is_error,
                            "tool_name": tool_name,
                        }));
                    }
                }
                rendered.push(json!({
                    "role": "user",
                    "content": content,
                }));
            }
        }
    }

    (system_lines.join("\n\n"), rendered)
}

fn parse_anthropic_usage(usage: &Value) -> Option<Usage> {
    let input_tokens = usage
        .get("input_tokens")
        .and_then(Value::as_u64)
        .and_then(|tokens| u32::try_from(tokens).ok());
    let output_tokens = usage
        .get("output_tokens")
        .and_then(Value::as_u64)
        .and_then(|tokens| u32::try_from(tokens).ok());
    let total_tokens = match (input_tokens, output_tokens) {
        (Some(input), Some(output)) => input.checked_add(output),
        _ => None,
    };
    if input_tokens.is_none() && output_tokens.is_none() && total_tokens.is_none() {
        return None;
    }
    Some(Usage {
        input_tokens,
        output_tokens,
        total_tokens,
    })
}

#[derive(Clone, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn finalize_tool_call(index: u64, partial: &PartialToolCall) -> Result<ToolCall, MuAiError> {
    let id = partial
        .id
        .clone()
        .unwrap_or_else(|| format!("tool_{index}"));
    let name = partial
        .name
        .clone()
        .ok_or_else(|| MuAiError::InvalidToolArguments(format!("tool call {id} missing name")))?;
    let arguments = if partial.arguments.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&partial.arguments).map_err(|error| {
            MuAiError::InvalidToolArguments(format!("failed to parse arguments for {id}: {error}"))
        })?
    };

    Ok(ToolCall {
        id,
        name,
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use futures::TryStreamExt;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::models::{ModelSpec, ProviderId};
    use crate::{AssistantEvent, ChatProvider, Message, Role, StreamRequest, ToolSpec};

    use super::AnthropicProvider;

    #[tokio::test]
    async fn normalizes_anthropic_streams() {
        let server = MockServer::start().await;
        std::env::set_var("MU_ANTHROPIC_API_KEY", "test-key");
        std::env::set_var("MU_ANTHROPIC_BASE_URL", server.uri());

        let body = concat!(
            "event: message_start\n",
            "data: {\"message\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":0}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"write\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"README.md\\\",\\\"content\\\":\\\"hi\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {\"index\":1}\n\n",
            "event: message_delta\n",
            "data: {\"usage\":{\"input_tokens\":10,\"output_tokens\":3}}\n\n",
            "event: message_stop\n",
            "data: {}\n\n"
        );

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("x-api-key", "test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = AnthropicProvider::default();
        let stream = match provider
            .stream(StreamRequest {
                model: ModelSpec::new(
                    ProviderId::Anthropic,
                    "claude-3-5-sonnet-latest",
                    "Claude Sonnet",
                    200_000,
                    8_192,
                ),
                messages: vec![Message::text(Role::User, "say hi")],
                tools: vec![ToolSpec {
                    name: "write".to_string(),
                    description: "Write a file".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" },
                            "content": { "type": "string" }
                        },
                        "required": ["path", "content"]
                    }),
                }],
                max_tokens: Some(256),
                temperature: Some(0.0),
            })
            .await
        {
            Ok(value) => value,
            Err(error) => panic!("expected stream, got error: {error}"),
        };

        let events = match stream.try_collect::<Vec<_>>().await {
            Ok(value) => value,
            Err(error) => panic!("expected events, got error: {error}"),
        };

        assert!(events
            .iter()
            .any(|event| matches!(event, AssistantEvent::TextDelta { delta } if delta == "Hello")));
        assert!(events.iter().any(|event| matches!(event, AssistantEvent::Usage { usage } if usage.total_tokens == Some(13))));
        assert!(events.iter().any(|event| matches!(event, AssistantEvent::ToolCall { call } if call.name == "write" && call.arguments == json!({"path": "README.md", "content": "hi"}))));
    }
}
