use std::collections::{BTreeMap, BTreeSet};

use async_stream::try_stream;
use async_trait::async_trait;
use futures::TryStreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

use crate::models::ProviderId;
use crate::sse::stream_sse_frames;
use crate::{
    load_provider_credentials, AssistantEvent, AssistantEventStream, ChatProvider, ContentPart,
    Message, MuAiError, StopReason, StreamRequest, ToolCall, Usage,
};

#[derive(Clone)]
pub struct OpenAiCompatibleProvider {
    client: reqwest::Client,
}

impl Default for OpenAiCompatibleProvider {
    fn default() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl ChatProvider for OpenAiCompatibleProvider {
    async fn stream(&self, request: StreamRequest) -> Result<AssistantEventStream, MuAiError> {
        let credentials = load_provider_credentials(&ProviderId::OpenAiCompatible)?;
        let body = build_openai_request_body(&request);

        let endpoint = format!(
            "{}/chat/completions",
            credentials.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(endpoint)
            .header(AUTHORIZATION, format!("Bearer {}", credentials.api_key))
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
                if frame.data == "[DONE]" {
                    for event in from_partial_calls(&partial_calls, &mut emitted_calls)? {
                        yield event;
                    }
                    yield AssistantEvent::Stop { reason: StopReason::EndTurn };
                    continue;
                }

                let payload: Value = serde_json::from_str(&frame.data)?;
                if let Some(usage) = parse_openai_usage(&payload) {
                    yield AssistantEvent::Usage { usage };
                }

                let Some(choice) = payload.get("choices").and_then(Value::as_array).and_then(|choices| choices.first()) else {
                    continue;
                };

                if let Some(text) = choice
                    .get("delta")
                    .and_then(|delta| delta.get("content"))
                    .and_then(Value::as_str)
                {
                    if !text.is_empty() {
                        yield AssistantEvent::TextDelta {
                            delta: text.to_string(),
                        };
                    }
                }

                if let Some(tool_calls) = choice
                    .get("delta")
                    .and_then(|delta| delta.get("tool_calls"))
                    .and_then(Value::as_array)
                {
                    for tool_call in tool_calls {
                        let Some(index) = tool_call.get("index").and_then(Value::as_u64) else {
                            continue;
                        };
                        let entry = partial_calls.entry(index).or_default();

                        if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                            entry.id = Some(id.to_string());
                        }
                        if let Some(name) = tool_call
                            .get("function")
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str)
                        {
                            entry.name = Some(name.to_string());
                        }
                        if let Some(arguments) = tool_call
                            .get("function")
                            .and_then(|function| function.get("arguments"))
                            .and_then(Value::as_str)
                        {
                            entry.arguments.push_str(arguments);
                            yield AssistantEvent::ToolCallDelta {
                                id: entry
                                    .id
                                    .clone()
                                    .unwrap_or_else(|| format!("call_{index}")),
                                name: entry.name.clone(),
                                partial_json: arguments.to_string(),
                            };
                        }
                    }
                }

                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    if matches!(reason, "tool_calls" | "stop" | "length") {
                        for event in from_partial_calls(&partial_calls, &mut emitted_calls)? {
                            yield event;
                        }
                    }

                    let stop_reason = match reason {
                        "tool_calls" => StopReason::ToolCall,
                        "length" => StopReason::MaxTokens,
                        "stop" => StopReason::EndTurn,
                        other => StopReason::Unknown(other.to_string()),
                    };
                    yield AssistantEvent::Stop { reason: stop_reason };
                }
            }
        };

        Ok(Box::pin(stream))
    }
}

fn build_openai_request_body(request: &StreamRequest) -> Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "model".to_string(),
        Value::String(request.model.id.0.clone()),
    );
    body.insert(
        "messages".to_string(),
        Value::Array(to_openai_messages(&request.messages)),
    );
    body.insert(
        "tools".to_string(),
        Value::Array(
            request
                .tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.input_schema,
                        }
                    })
                })
                .collect::<Vec<_>>(),
        ),
    );
    body.insert("stream".to_string(), Value::Bool(true));
    body.insert(
        "stream_options".to_string(),
        json!({ "include_usage": true }),
    );
    if let Some(temperature) = request.temperature {
        body.insert("temperature".to_string(), json!(temperature));
    }

    if let Some(max_tokens) = request.max_tokens {
        let key = if uses_max_completion_tokens(&request.model.id.0) {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        body.insert(key.to_string(), json!(max_tokens));
    }

    Value::Object(body)
}

fn uses_max_completion_tokens(model_id: &str) -> bool {
    model_id.starts_with("gpt-5")
}

fn to_openai_messages(messages: &[Message]) -> Vec<Value> {
    let mut rendered = Vec::new();
    for message in messages {
        match message.role {
            crate::Role::System | crate::Role::User => {
                rendered.push(json!({
                    "role": match message.role {
                        crate::Role::System => "system",
                        crate::Role::User => "user",
                        _ => unreachable!(),
                    },
                    "content": message.plain_text(),
                }));
            }
            crate::Role::Assistant => {
                let text = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let tool_calls = message
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        ContentPart::ToolCall(call) => Some(json!({
                            "id": call.id,
                            "type": "function",
                            "function": {
                                "name": call.name,
                                "arguments": call.arguments.to_string(),
                            }
                        })),
                        _ => None,
                    })
                    .collect::<Vec<_>>();
                let content = if text.is_empty() {
                    Value::Null
                } else {
                    Value::String(text)
                };
                rendered.push(json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tool_calls,
                }));
            }
            crate::Role::Tool => {
                for part in &message.content {
                    if let ContentPart::ToolResult {
                        tool_call_id,
                        result,
                        ..
                    } = part
                    {
                        rendered.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call_id,
                            "content": result,
                        }));
                    }
                }
            }
        }
    }
    rendered
}

fn parse_openai_usage(value: &Value) -> Option<Usage> {
    let usage = value.get("usage")?;
    Some(Usage {
        input_tokens: usage
            .get("prompt_tokens")
            .and_then(Value::as_u64)
            .and_then(|tokens| u32::try_from(tokens).ok()),
        output_tokens: usage
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .and_then(|tokens| u32::try_from(tokens).ok()),
        total_tokens: usage
            .get("total_tokens")
            .and_then(Value::as_u64)
            .and_then(|tokens| u32::try_from(tokens).ok()),
    })
}

#[derive(Clone, Default)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

fn from_partial_calls(
    partial_calls: &BTreeMap<u64, PartialToolCall>,
    emitted_calls: &mut BTreeSet<u64>,
) -> Result<Vec<AssistantEvent>, MuAiError> {
    let mut events = Vec::new();
    for (index, call) in partial_calls {
        if emitted_calls.contains(index) {
            continue;
        }
        emitted_calls.insert(*index);
        events.push(AssistantEvent::ToolCall {
            call: finalize_tool_call(*index, call)?,
        });
    }
    Ok(events)
}

fn finalize_tool_call(index: u64, partial: &PartialToolCall) -> Result<ToolCall, MuAiError> {
    let id = partial
        .id
        .clone()
        .unwrap_or_else(|| format!("call_{index}"));
    let name = partial.name.clone().ok_or_else(|| {
        MuAiError::InvalidToolArguments(format!("tool call {id} missing function name"))
    })?;
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

    use super::{build_openai_request_body, OpenAiCompatibleProvider};

    #[tokio::test]
    async fn assembles_streamed_tool_calls() {
        let server = MockServer::start().await;
        std::env::set_var("MU_OPENAI_API_KEY", "test-key");
        std::env::set_var("MU_OPENAI_BASE_URL", server.uri());

        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"src/lib.rs\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":4,\"total_tokens\":9}}\n\n",
            "data: [DONE]\n\n"
        );

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(body)
                    .insert_header("content-type", "text/event-stream"),
            )
            .mount(&server)
            .await;

        let provider = OpenAiCompatibleProvider::default();
        let stream = match provider
            .stream(StreamRequest {
                model: ModelSpec::new(
                    ProviderId::OpenAiCompatible,
                    "gpt-4o-mini",
                    "GPT-4o mini",
                    128_000,
                    16_384,
                ),
                messages: vec![Message::text(Role::User, "hi")],
                tools: vec![ToolSpec {
                    name: "read".to_string(),
                    description: "Read a file".to_string(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
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
            .any(|event| matches!(event, AssistantEvent::TextDelta { delta } if delta == "Hi")));
        assert!(events.iter().any(|event| matches!(event, AssistantEvent::Usage { usage } if usage.total_tokens == Some(9))));
        assert!(events.iter().any(|event| matches!(event, AssistantEvent::ToolCall { call } if call.name == "read" && call.arguments == json!({"path": "src/lib.rs"}))));
    }

    #[test]
    fn uses_max_tokens_for_non_gpt_5_models() {
        let body = build_openai_request_body(&StreamRequest {
            model: ModelSpec::new(
                ProviderId::OpenAiCompatible,
                "gpt-4o-mini",
                "GPT-4o mini",
                128_000,
                16_384,
            ),
            messages: vec![Message::text(Role::User, "hi")],
            tools: Vec::new(),
            max_tokens: Some(256),
            temperature: Some(0.0),
        });

        assert_eq!(body.get("max_tokens"), Some(&json!(256)));
        assert!(body.get("max_completion_tokens").is_none());
    }

    #[test]
    fn uses_max_completion_tokens_for_gpt_5_models() {
        let body = build_openai_request_body(&StreamRequest {
            model: ModelSpec::new(
                ProviderId::OpenAiCompatible,
                "gpt-5.4",
                "GPT-5.4",
                1_000_000,
                100_000,
            ),
            messages: vec![Message::text(Role::User, "hi")],
            tools: Vec::new(),
            max_tokens: Some(256),
            temperature: Some(0.0),
        });

        assert_eq!(body.get("max_completion_tokens"), Some(&json!(256)));
        assert!(body.get("max_tokens").is_none());
    }
}
