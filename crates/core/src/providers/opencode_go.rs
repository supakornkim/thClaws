//! OpenCodeGo (opencode.ai) streaming provider.
//!
//! OpenCodeGo is a subscription gateway that serves models via 3 wire formats
//! through a single base URL:
//!
//! - **OpenAI-compatible** (`/chat/completions`): GLM, Kimi, DeepSeek, MiMo
//! - **Anthropic-compatible** (`/messages`): MiniMax M2.5, M2.7
//! - **Alibaba-compatible** (`/chat/completions`): Qwen3.5/3.6 Plus
//!
//! The provider detects the wire format from the model id (after stripping the
//! `opencode-go/` prefix) and routes to the correct endpoint/parser.

use super::{EventStream, ModelInfo, Provider, ProviderEvent, StreamRequest, Usage};
use crate::error::{Error, Result};
use crate::types::{ContentBlock, ImageSource, Role, ToolResultBlock, ToolResultContent};
use async_stream::try_stream;
use async_trait::async_trait;
use futures::StreamExt;
use reqwest::Client;
use serde_json::{json, Value};

pub const DEFAULT_API_URL: &str = "https://opencode.ai/zen/go/v1";
pub const MODELS_URL: &str = "https://opencode.ai/zen/go/v1/models";

// Wire-format routing tables. These are hardcoded today; longer-term
// we should probe opencode.ai's `/v1/models` for a per-model `wire`
// hint and discover routing live — that way a new minimax-m3.0 or
// qwen4.x release doesn't silently fall through to the OpenAI path
// and get rejected with a 4xx upstream. Tracked as a TODO so we can
// rip out the lists once the upstream exposes the hint.
const ANTHROPIC_MODELS: &[&str] = &["minimax-m2.5", "minimax-m2.7"];
const ALIBABA_MODELS: &[&str] = &["qwen3.5-plus", "qwen3.6-plus"];
const REASONING_MODELS: &[&str] = &["deepseek-v4-pro", "deepseek-v4-flash"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WireFormat {
    OpenAI,
    Anthropic,
    /// Reserved for opencode.ai's qwen3.5/3.6-plus models. The
    /// upstream currently accepts them through the standard
    /// `/chat/completions` OpenAI-compat endpoint, so request shape
    /// + endpoint + auth are byte-identical to `OpenAI` today — the
    /// variant exists as a forward-looking placeholder for the day
    /// opencode.ai switches qwen routing to DashScope's native
    /// `/services/aigc/text-generation/generation` shape. If you're
    /// changing this code and the equivalence still holds, leaving
    /// the variant in place is fine; if it diverges, branch the
    /// body / endpoint / auth here.
    Alibaba,
}

fn detect_wire_format(model: &str) -> WireFormat {
    let lower = model.to_lowercase();
    if ANTHROPIC_MODELS.contains(&lower.as_str()) {
        WireFormat::Anthropic
    } else if ALIBABA_MODELS.contains(&lower.as_str()) {
        WireFormat::Alibaba
    } else {
        WireFormat::OpenAI
    }
}

fn endpoint_for(format: WireFormat) -> &'static str {
    match format {
        WireFormat::OpenAI => "/chat/completions",
        WireFormat::Alibaba => "/chat/completions",
        WireFormat::Anthropic => "/messages",
    }
}

fn model_uses_reasoning_content(model: &str) -> bool {
    let lower = model.to_lowercase();
    REASONING_MODELS.iter().any(|p| lower.contains(p))
}

pub struct OpencodeGoProvider {
    client: Client,
    api_key: String,
    base_url: String,
    list_models_url: Option<String>,
}

impl OpencodeGoProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: Client::new(),
            api_key: api_key.into(),
            base_url: DEFAULT_API_URL.to_string(),
            list_models_url: None,
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_list_models_url(mut self, url: impl Into<String>) -> Self {
        self.list_models_url = Some(url.into());
        self
    }

    fn messages_to_openai(req: &StreamRequest) -> Vec<Value> {
        let mut out: Vec<Value> = Vec::new();
        let echo_reasoning = model_uses_reasoning_content(&req.model);

        if let Some(sys) = &req.system {
            if !sys.is_empty() {
                out.push(json!({"role": "system", "content": sys}));
            }
        }

        for m in &req.messages {
            let role = match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::System => "system",
            };

            let mut text_parts: Vec<String> = Vec::new();
            let mut thinking_parts: Vec<String> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            let mut trailing_tool_results: Vec<(String, String, Vec<(String, String)>)> =
                Vec::new();
            let mut inline_user_images: Vec<(String, String)> = Vec::new();

            for block in &m.content {
                match block {
                    ContentBlock::Text { text } => text_parts.push(text.clone()),
                    ContentBlock::Thinking { content, .. } => {
                        if echo_reasoning {
                            thinking_parts.push(content.clone());
                        }
                    }
                    ContentBlock::Image {
                        source: ImageSource::Base64 { media_type, data },
                    } => {
                        inline_user_images.push((media_type.clone(), data.clone()));
                    }
                    ContentBlock::ToolUse {
                        id, name, input, ..
                    } => {
                        let args = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
                        tool_calls.push(json!({
                            "id": id, "type": "function",
                            "function": { "name": name, "arguments": args },
                        }));
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        let text = content.to_text();
                        let images = extract_images(content);
                        trailing_tool_results.push((tool_use_id.clone(), text, images));
                    }
                }
            }

            let content_text = text_parts.join("");
            let reasoning_text = thinking_parts.join("");
            let has_text = !content_text.is_empty();
            let has_reasoning = !reasoning_text.is_empty();
            let has_tools = !tool_calls.is_empty();
            let has_inline_images = !inline_user_images.is_empty();

            if has_text || has_tools || has_reasoning || has_inline_images {
                let mut msg = json!({"role": role});
                if has_inline_images {
                    let mut content_arr: Vec<Value> = Vec::new();
                    if has_text {
                        content_arr.push(json!({"type": "text", "text": content_text}));
                    }
                    for (media_type, data) in &inline_user_images {
                        content_arr.push(json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{media_type};base64,{data}") }
                        }));
                    }
                    msg["content"] = json!(content_arr);
                } else if has_text {
                    msg["content"] = json!(content_text);
                } else if has_tools {
                    msg["content"] = Value::Null;
                }
                if has_tools {
                    msg["tool_calls"] = json!(tool_calls);
                }
                if has_reasoning {
                    msg["reasoning_content"] = json!(reasoning_text);
                }
                out.push(msg);
            }

            for (tool_call_id, content, _images) in &trailing_tool_results {
                out.push(json!({
                    "role": "tool", "tool_call_id": tool_call_id, "content": content,
                }));
            }

            let total_images: usize = trailing_tool_results.iter().map(|(_, _, i)| i.len()).sum();
            if total_images > 0 {
                let mut user_content: Vec<Value> = Vec::with_capacity(total_images * 2 + 1);
                let call_ids: Vec<&str> = trailing_tool_results
                    .iter()
                    .filter(|(_, _, i)| !i.is_empty())
                    .map(|(id, _, _)| id.as_str())
                    .collect();
                user_content.push(json!({
                    "type": "text",
                    "text": format!(
                        "(image{} attached from preceding tool_result{}: {})",
                        if total_images == 1 { "" } else { "s" },
                        if call_ids.len() == 1 { "" } else { "s" },
                        call_ids.join(", ")
                    ),
                }));
                for (tool_call_id, _content, images) in &trailing_tool_results {
                    for (media_type, data) in images {
                        user_content
                            .push(json!({"type": "text", "text": format!("from {tool_call_id}:")}));
                        user_content.push(json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{media_type};base64,{data}") }
                        }));
                    }
                }
                out.push(json!({"role": "user", "content": user_content}));
            }
        }
        out
    }

    fn build_openai_body(req: &StreamRequest) -> Value {
        let messages = Self::messages_to_openai(req);
        let mut body = json!({
            "model": req.model,
            "max_completion_tokens": req.max_tokens,
            "messages": messages,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        if !req.tools.is_empty() {
            body["tools"] = json!(req.tools.iter().map(|t| json!({
                "type": "function",
                "function": { "name": t.name, "description": t.description, "parameters": t.input_schema }
            })).collect::<Vec<_>>());
        }
        body
    }

    fn build_anthropic_body(req: &StreamRequest) -> Value {
        let msgs: Vec<Value> = req.messages.iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant", _ => unreachable!() },
                "content": m.content,
            })).collect();

        let mut body = json!({
            "model": req.model, "max_tokens": req.max_tokens, "messages": msgs, "stream": true,
        });
        if let Some(sys) = &req.system {
            if !sys.is_empty() {
                body["system"] = json!([{"type": "text", "text": sys}]);
            }
        }
        if !req.tools.is_empty() {
            body["tools"] = json!(req.tools);
        }
        body
    }

    async fn send_request(
        &self,
        url: &str,
        body: &Value,
        wire_format: WireFormat,
    ) -> Result<reqwest::Response> {
        let req = self
            .client
            .post(url)
            .header("content-type", "application/json");
        let req = match wire_format {
            WireFormat::Anthropic => req
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01"),
            _ => req.header("authorization", format!("Bearer {}", self.api_key)),
        };
        req.json(body)
            .send()
            .await
            .map_err(|e| Error::Provider(format!("http: {e}")))
    }

    fn parse_openai_chunk(text: &str, state: &mut OpenaiParseState) -> Result<Vec<ProviderEvent>> {
        let text = text.trim();
        if text.is_empty() || text == "data: [DONE]" {
            return Ok(Vec::new());
        }
        let Some(data) = text
            .strip_prefix("data: ")
            .or_else(|| text.strip_prefix("data:"))
        else {
            return Ok(Vec::new());
        };
        let v: Value =
            serde_json::from_str(data).map_err(|e| Error::Provider(format!("json parse: {e}")))?;

        if v.get("error").is_some() {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("upstream error");
            return Err(Error::Provider(format!("upstream error: {msg}")));
        }

        let mut events = Vec::new();

        if !state.seen_message_start {
            state.seen_message_start = true;
            let model = v
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            events.push(ProviderEvent::MessageStart { model });
        }

        if let Some(choices) = v.get("choices").and_then(Value::as_array) {
            for choice in choices {
                let delta = choice.get("delta");
                if let Some(tool_calls) = delta
                    .and_then(|d| d.get("tool_calls"))
                    .and_then(Value::as_array)
                {
                    for tc in tool_calls {
                        let index = tc.get("index").and_then(Value::as_i64);
                        // OpenAI streams the tool_call `id` + `function.name`
                        // only on the first delta chunk for that call; later
                        // chunks carry just `function.arguments` deltas.
                        // Extract id only when the name is present so a mid-
                        // stream chunk with a stray `id` field doesn't restart
                        // the call.
                        let name = tc
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let id = if !name.is_empty() {
                            tc.get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string()
                        } else {
                            String::new()
                        };
                        if let Some(idx) = index {
                            if state.active_tool_index.is_some()
                                && state.active_tool_index != Some(idx)
                            {
                                events.push(ProviderEvent::ContentBlockStop);
                            }
                            state.active_tool_index = Some(idx);
                        }
                        if !id.is_empty() || !name.is_empty() {
                            events.push(ProviderEvent::ToolUseStart {
                                id,
                                name,
                                thought_signature: None,
                            });
                        }
                        let args = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if !args.is_empty() {
                            events.push(ProviderEvent::ToolUseDelta {
                                partial_json: args.to_string(),
                            });
                        }
                    }
                }

                if let Some(content) = delta.and_then(|d| d.get("content")).and_then(Value::as_str)
                {
                    if !content.is_empty() {
                        events.push(ProviderEvent::TextDelta(content.to_string()));
                    }
                }
                if let Some(reasoning) = delta
                    .and_then(|d| d.get("reasoning_content"))
                    .and_then(Value::as_str)
                {
                    if !reasoning.is_empty() {
                        events.push(ProviderEvent::ThinkingDelta(reasoning.to_string()));
                    }
                }

                if let Some(finish) = choice.get("finish_reason").and_then(Value::as_str) {
                    if state.active_tool_index.is_some() {
                        events.push(ProviderEvent::ContentBlockStop);
                        state.active_tool_index = None;
                    }
                    let usage = v.get("usage").map(|u| Usage {
                        input_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0)
                            as u32,
                        output_tokens: u
                            .get("completion_tokens")
                            .and_then(Value::as_u64)
                            .unwrap_or(0) as u32,
                        cache_creation_input_tokens: u
                            .get("prompt_tokens_details")
                            .and_then(|d| d.get("cache_creation_tokens"))
                            .and_then(Value::as_u64)
                            .map(|n| n as u32),
                        cache_read_input_tokens: u
                            .get("prompt_tokens_details")
                            .and_then(|d| d.get("cached_tokens"))
                            .and_then(Value::as_u64)
                            .map(|n| n as u32),
                    });
                    if !state.emitted_message_stop {
                        state.emitted_message_stop = true;
                        events.push(ProviderEvent::MessageStop {
                            stop_reason: Some(finish.to_string()),
                            usage,
                        });
                    }
                }
            }
        }

        if v.get("choices")
            .and_then(Value::as_array)
            .map_or(true, |c| c.is_empty())
            && v.get("usage").is_some()
            && state.seen_message_start
        {
            let usage = v.get("usage").map(|u| Usage {
                input_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
                output_tokens: u
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            });
            if !state.emitted_message_stop {
                state.emitted_message_stop = true;
                events.push(ProviderEvent::MessageStop {
                    stop_reason: Some("stop".to_string()),
                    usage,
                });
            }
        }

        Ok(events)
    }

    fn parse_anthropic_chunk(text: &str) -> Result<Option<ProviderEvent>> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        let mut event_type: Option<&str> = None;
        let mut data_line: Option<&str> = None;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("event: ") {
                event_type = Some(rest);
            } else if let Some(rest) = line.strip_prefix("data: ") {
                data_line = Some(rest);
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_line = Some(rest);
            }
        }
        let Some(data) = data_line else {
            return Ok(None);
        };
        if event_type == Some("ping") || event_type == Some("message_stop") {
            return Ok(None);
        }
        let v: Value =
            serde_json::from_str(data).map_err(|e| Error::Provider(format!("json parse: {e}")))?;
        if v.get("error").is_some() {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("upstream error");
            return Err(Error::Provider(format!("upstream error: {msg}")));
        }
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        match ty {
            "message_start" => {
                let model = v
                    .pointer("/message/model")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                Ok(Some(ProviderEvent::MessageStart { model }))
            }
            "content_block_start" => {
                let cb = v.get("content_block");
                let cb_type = cb
                    .and_then(|c| c.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if cb_type == "tool_use" {
                    let id = cb
                        .and_then(|c| c.get("id"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let name = cb
                        .and_then(|c| c.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    Ok(Some(ProviderEvent::ToolUseStart {
                        id,
                        name,
                        thought_signature: None,
                    }))
                } else {
                    Ok(None)
                }
            }
            "content_block_delta" => {
                let delta = v.get("delta");
                let dt = delta
                    .and_then(|d| d.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                match dt {
                    "text_delta" => {
                        let text = delta
                            .and_then(|d| d.get("text"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        Ok(Some(ProviderEvent::TextDelta(text)))
                    }
                    "input_json_delta" => {
                        let pj = delta
                            .and_then(|d| d.get("partial_json"))
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        Ok(Some(ProviderEvent::ToolUseDelta { partial_json: pj }))
                    }
                    _ => Ok(None),
                }
            }
            "content_block_stop" => Ok(Some(ProviderEvent::ContentBlockStop)),
            "message_delta" => {
                let delta = v.get("delta");
                let stop_reason = delta
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                    .map(String::from);
                let usage = v.get("usage").map(|u| Usage {
                    input_tokens: u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32,
                    output_tokens: u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0)
                        as u32,
                    cache_creation_input_tokens: u
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64)
                        .map(|n| n as u32),
                    cache_read_input_tokens: u
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_u64)
                        .map(|n| n as u32),
                });
                Ok(Some(ProviderEvent::MessageStop { stop_reason, usage }))
            }
            _ => Ok(None),
        }
    }

    fn models_list_url(&self) -> String {
        if let Some(url) = &self.list_models_url {
            return url.clone();
        }
        format!("{}/models", self.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl Provider for OpencodeGoProvider {
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        let models_url = self.models_list_url();
        let resp = self
            .client
            .get(&models_url)
            .header("authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| Error::Provider(format!("http: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!(
                "http {status}: {}",
                super::redact_key(&text, &self.api_key)
            )));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| Error::Provider(format!("json: {e}")))?;
        let prefix = "opencode-go/";
        let mut out: Vec<ModelInfo> = v
            .get("data")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let raw = m.get("id").and_then(Value::as_str)?;
                        let id = if raw.starts_with(prefix) {
                            raw.to_string()
                        } else {
                            format!("{prefix}{raw}")
                        };
                        Some(ModelInfo {
                            id,
                            display_name: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    async fn stream(&self, mut req: StreamRequest) -> Result<EventStream> {
        if let Some(rest) = req.model.strip_prefix("opencode-go/") {
            req.model = rest.to_string();
        }

        let wire_format = detect_wire_format(&req.model);
        let endpoint = format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            endpoint_for(wire_format)
        );
        let body = match wire_format {
            WireFormat::OpenAI | WireFormat::Alibaba => Self::build_openai_body(&req),
            WireFormat::Anthropic => Self::build_anthropic_body(&req),
        };

        let resp = self.send_request(&endpoint, &body, wire_format).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Provider(format!(
                "http {status}: {}",
                super::redact_key(&text, &self.api_key)
            )));
        }

        let byte_stream = resp.bytes_stream();
        let raw_dump = super::RawDump::new(format!("opencodego {}", req.model));
        let chunk_timeout = req
            .stream_chunk_timeout_override
            .unwrap_or_else(super::stream_chunk_timeout);

        let event_stream = try_stream! {
            let mut buffer: Vec<u8> = Vec::new();
            let mut byte_stream = Box::pin(byte_stream);
            match wire_format {
                WireFormat::OpenAI | WireFormat::Alibaba => {
                    let mut state = OpenaiParseState::default();
                    let mut raw = raw_dump;
                    loop {
                        let maybe_chunk = tokio::time::timeout(chunk_timeout, byte_stream.next())
                            .await.map_err(|_| Error::Provider(format!("stream idle for {}s — provider stopped sending; try again", chunk_timeout.as_secs())))?;
                        let Some(chunk) = maybe_chunk else { break };
                        let chunk = chunk.map_err(|e| Error::Provider(format!("stream: {e}")))?;
                        buffer.extend_from_slice(&chunk);
                        while let Some(boundary) = super::find_bytes(&buffer, b"\n\n") {
                            let event_bytes: Vec<u8> = buffer.drain(..boundary + 2).collect();
                            let event_text = String::from_utf8_lossy(&event_bytes);
                            let trimmed = event_text.trim_end_matches('\n');
                            for event in Self::parse_openai_chunk(trimmed, &mut state)? {
                                if let ProviderEvent::TextDelta(ref s) = event { raw.push(s); }
                                yield event;
                            }
                        }
                    }
                    for event in state.flush_eof() { yield event; }
                    raw.flush();
                }
                WireFormat::Anthropic => {
                    let mut raw = raw_dump;
                    loop {
                        let maybe_chunk = tokio::time::timeout(chunk_timeout, byte_stream.next())
                            .await.map_err(|_| Error::Provider(format!("stream idle for {}s — provider stopped sending; try again", chunk_timeout.as_secs())))?;
                        let Some(chunk) = maybe_chunk else { break };
                        let chunk = chunk.map_err(|e| Error::Provider(format!("stream: {e}")))?;
                        buffer.extend_from_slice(&chunk);
                        while let Some(boundary) = super::find_bytes(&buffer, b"\n\n") {
                            let event_bytes: Vec<u8> = buffer.drain(..boundary + 2).collect();
                            let event_text = String::from_utf8_lossy(&event_bytes);
                            let trimmed = event_text.trim_end_matches('\n');
                            if let Some(ev) = Self::parse_anthropic_chunk(trimmed)? {
                                if let ProviderEvent::TextDelta(ref s) = ev { raw.push(s); }
                                yield ev;
                            }
                        }
                    }
                    raw.flush();
                }
            }
        };

        Ok(Box::pin(event_stream))
    }
}

#[derive(Default, Debug)]
struct OpenaiParseState {
    seen_message_start: bool,
    active_tool_index: Option<i64>,
    emitted_message_stop: bool,
}

impl OpenaiParseState {
    fn flush_eof(&mut self) -> Vec<ProviderEvent> {
        let mut out = Vec::new();
        if self.active_tool_index.is_some() {
            out.push(ProviderEvent::ContentBlockStop);
            self.active_tool_index = None;
        }
        out
    }
}

fn extract_images(content: &ToolResultContent) -> Vec<(String, String)> {
    match content {
        ToolResultContent::Text(_) => Vec::new(),
        ToolResultContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|b| match b {
                ToolResultBlock::Image {
                    source: ImageSource::Base64 { media_type, data },
                } => Some((media_type.clone(), data.clone())),
                _ => None,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Message;

    #[test]
    fn test_detect_wire_format_openai() {
        assert_eq!(detect_wire_format("glm-5.1"), WireFormat::OpenAI);
        assert_eq!(detect_wire_format("kimi-k2.6"), WireFormat::OpenAI);
        assert_eq!(detect_wire_format("deepseek-v4-flash"), WireFormat::OpenAI);
        assert_eq!(detect_wire_format("mimo-v2.5"), WireFormat::OpenAI);
    }

    #[test]
    fn test_detect_wire_format_anthropic() {
        assert_eq!(detect_wire_format("minimax-m2.7"), WireFormat::Anthropic);
        assert_eq!(detect_wire_format("minimax-m2.5"), WireFormat::Anthropic);
    }

    #[test]
    fn test_detect_wire_format_alibaba() {
        assert_eq!(detect_wire_format("qwen3.6-plus"), WireFormat::Alibaba);
        assert_eq!(detect_wire_format("qwen3.5-plus"), WireFormat::Alibaba);
    }

    #[test]
    fn test_endpoint_for() {
        assert_eq!(endpoint_for(WireFormat::OpenAI), "/chat/completions");
        assert_eq!(endpoint_for(WireFormat::Alibaba), "/chat/completions");
        assert_eq!(endpoint_for(WireFormat::Anthropic), "/messages");
    }

    #[test]
    fn test_model_uses_reasoning_content() {
        assert!(model_uses_reasoning_content("deepseek-v4-flash"));
        assert!(model_uses_reasoning_content("deepseek-v4-pro"));
        assert!(!model_uses_reasoning_content("glm-5.1"));
    }

    #[test]
    fn test_openai_parse_text_delta() {
        let mut state = OpenaiParseState::default();
        let text = r#"data: {"model":"glm-5.1","choices":[{"index":0,"delta":{"role":"assistant","content":"Hello"}}]}"#;
        let events = OpencodeGoProvider::parse_openai_chunk(text, &mut state).unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], ProviderEvent::MessageStart { .. }));
        assert!(matches!(&events[1], ProviderEvent::TextDelta(_)));
    }

    #[test]
    fn test_anthropic_parse_text_delta() {
        let text = r#"event: content_block_delta
data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let event = OpencodeGoProvider::parse_anthropic_chunk(text)
            .unwrap()
            .unwrap();
        assert!(matches!(event, ProviderEvent::TextDelta(_)));
    }

    #[test]
    fn test_anthropic_skip_ping() {
        let text = "event: ping\ndata: {}";
        let event = OpencodeGoProvider::parse_anthropic_chunk(text).unwrap();
        assert!(event.is_none());
    }

    #[test]
    fn test_provider_default_url() {
        let provider = OpencodeGoProvider::new("test-key");
        assert_eq!(provider.base_url, DEFAULT_API_URL);
    }

    #[test]
    fn test_provider_custom_url() {
        let provider =
            OpencodeGoProvider::new("test-key").with_base_url("https://custom.example.com");
        assert_eq!(provider.base_url, "https://custom.example.com");
    }

    /// Integration test: streams from minimax-m2.7 (Anthropic wire format with x-api-key).
    /// Requires OPENCODE_GO_API_KEY env var.
    #[tokio::test]
    #[ignore = "requires OPENCODE_GO_API_KEY"]
    async fn it_streams_minimax_m2_7() {
        let key = std::env::var("OPENCODE_GO_API_KEY").expect("OPENCODE_GO_API_KEY must be set");
        let provider = OpencodeGoProvider::new(key);
        let req = StreamRequest {
            model: "opencode-go/minimax-m2.7".into(),
            system: None,
            messages: vec![Message::user("Say hello in one word")],
            tools: vec![],
            max_tokens: 30,
            thinking_budget: None,
            stream_chunk_timeout_override: Some(std::time::Duration::from_secs(10)),
        };
        let stream = provider.stream(req).await.unwrap();
        let mut count = 0;
        tokio::pin!(stream);
        while let Some(event) = stream.next().await {
            let _ = event.unwrap();
            count += 1;
        }
        assert!(count > 0, "expected at least one event");
    }

    /// Integration test: streams from qwen3.6-plus (Alibaba wire format).
    /// Requires OPENCODE_GO_API_KEY env var.
    #[tokio::test]
    #[ignore = "requires OPENCODE_GO_API_KEY"]
    async fn it_streams_qwen3_6_plus() {
        let key = std::env::var("OPENCODE_GO_API_KEY").expect("OPENCODE_GO_API_KEY must be set");
        let provider = OpencodeGoProvider::new(key);
        let req = StreamRequest {
            model: "opencode-go/qwen3.6-plus".into(),
            system: None,
            messages: vec![Message::user("Say hello in one word")],
            tools: vec![],
            max_tokens: 30,
            thinking_budget: None,
            stream_chunk_timeout_override: Some(std::time::Duration::from_secs(10)),
        };
        let stream = provider.stream(req).await.unwrap();
        let mut count = 0;
        tokio::pin!(stream);
        while let Some(event) = stream.next().await {
            let _ = event.unwrap();
            count += 1;
        }
        assert!(count > 0, "expected at least one event");
    }
}
