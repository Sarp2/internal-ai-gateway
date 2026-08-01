use std::io;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::read::DecoderReader;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::mock_control::MockResponseType;
use crate::mock_stream::{DelayedByteStream, split_text};
use crate::openai_control::{MockHttpError, OpenAiMockControl};

const MOCK_COMPLETION_ID: &str = "chatcmpl_mock_openai_001";
const MOCK_REQUEST_ID: &str = "req_mock_openai_001";
const MOCK_RESPONSE_TEXT: &str = "Mock OpenAI response.";
const MOCK_CREATED_AT: u64 = 1_700_000_000;
const MOCK_TOOL_CALL_ID: &str = "call_mock_weather_001";
const MOCK_TOOL_NAME: &str = "get_weather";
const MOCK_TOOL_ARGUMENTS: &str = r#"{"location":"Istanbul"}"#;
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 256 * 1024 * 1024;

#[derive(Deserialize)]
pub(crate) struct ChatCompletionsRequest {
    #[serde(default)]
    max_completion_tokens: Option<u64>,
    messages: Vec<ChatMessage>,
    model: String,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    stream_options: Option<StreamOptions>,
}

impl ChatCompletionsRequest {
    fn validate(&self) -> Result<(), &'static str> {
        if self.model.trim().is_empty() {
            return Err("model must not be empty");
        }
        if self.messages.is_empty() {
            return Err("messages must not be empty");
        }
        if self.messages.iter().any(|message| !message.is_valid()) {
            return Err("each message must have a valid role and content");
        }
        if self.max_completion_tokens == Some(0) {
            return Err("max_completion_tokens must be greater than zero");
        }

        Ok(())
    }

    fn include_stream_usage(&self) -> bool {
        self.stream
            && self
                .stream_options
                .as_ref()
                .is_some_and(|options| options.include_usage)
    }
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<MessageContent>,
    role: String,
    #[serde(default)]
    tool_calls: Option<Vec<ToolCall>>,
}

impl ChatMessage {
    fn is_valid(&self) -> bool {
        if !matches!(
            self.role.as_str(),
            "developer" | "system" | "user" | "assistant" | "tool"
        ) {
            return false;
        }

        let tool_calls_are_valid = match self.tool_calls.as_ref() {
            None => true,
            Some(tool_calls) => {
                self.role == "assistant"
                    && !tool_calls.is_empty()
                    && tool_calls.iter().all(ToolCall::is_valid)
            }
        };
        if !tool_calls_are_valid {
            return false;
        }

        match self.content.as_ref() {
            Some(MessageContent::Text(content)) => !content.trim().is_empty(),
            Some(MessageContent::Parts(content)) => {
                !content.is_empty()
                    && content
                        .iter()
                        .all(|part| part.is_valid_for_role(&self.role))
            }
            None => self.role == "assistant" && self.tool_calls.is_some(),
        }
    }
}

#[derive(Deserialize)]
struct ToolCall {
    function: ToolCallFunction,
    id: String,
    #[serde(rename = "type")]
    call_type: ToolCallType,
}

impl ToolCall {
    fn is_valid(&self) -> bool {
        !self.id.trim().is_empty()
            && matches!(self.call_type, ToolCallType::Function)
            && !self.function.name.trim().is_empty()
            && !self.function.arguments.trim().is_empty()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ToolCallType {
    Function,
}

#[derive(Deserialize)]
struct ToolCallFunction {
    arguments: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrlContent },
    InputAudio { input_audio: InputAudioContent },
    File { file: FileContent },
    Refusal { refusal: String },
}

impl ContentPart {
    fn is_valid_for_role(&self, role: &str) -> bool {
        match self {
            Self::Text { text } => {
                matches!(role, "developer" | "system" | "user" | "assistant" | "tool")
                    && !text.trim().is_empty()
            }
            Self::ImageUrl { image_url } => role == "user" && image_url.is_valid(),
            Self::InputAudio { input_audio } => role == "user" && input_audio.is_valid(),
            Self::File { file } => role == "user" && file.is_valid(),
            Self::Refusal { refusal } => role == "assistant" && !refusal.trim().is_empty(),
        }
    }
}

#[derive(Deserialize)]
struct ImageUrlContent {
    #[serde(default)]
    detail: Option<String>,
    url: String,
}

impl ImageUrlContent {
    fn is_valid(&self) -> bool {
        !self.url.trim().is_empty()
            && self
                .detail
                .as_deref()
                .is_none_or(|detail| matches!(detail, "auto" | "low" | "high"))
    }
}

#[derive(Deserialize)]
struct InputAudioContent {
    data: String,
    format: String,
}

impl InputAudioContent {
    fn is_valid(&self) -> bool {
        is_valid_base64(&self.data) && matches!(self.format.as_str(), "wav" | "mp3")
    }
}

#[derive(Deserialize)]
struct FileContent {
    #[serde(default)]
    file_data: Option<String>,
    #[serde(default)]
    file_id: Option<String>,
    #[serde(default)]
    filename: Option<String>,
}

impl FileContent {
    fn is_valid(&self) -> bool {
        match (self.file_id.as_deref(), self.file_data.as_deref()) {
            (Some(file_id), None) => !file_id.trim().is_empty(),
            (None, Some(file_data)) => {
                is_valid_base64(file_data)
                    && self
                        .filename
                        .as_deref()
                        .is_some_and(|filename| !filename.trim().is_empty())
            }
            _ => false,
        }
    }
}

fn is_valid_base64(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }

    let mut decoder = DecoderReader::new(value.as_bytes(), &BASE64_STANDARD);
    io::copy(&mut decoder, &mut io::sink()).is_ok()
}

#[derive(Deserialize)]
struct StreamOptions {
    #[serde(default)]
    include_usage: bool,
}

pub(crate) async fn chat_completions(
    headers: HeaderMap,
    payload: Result<Json<ChatCompletionsRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(request) => request,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return provider_error(MockHttpError::RequestTooLarge);
        }
        Err(_) => return invalid_request("request body must contain valid Chat Completions JSON"),
    };
    if let Err(message) = request.validate() {
        return invalid_request(message);
    }
    let control = match OpenAiMockControl::from_headers(&headers) {
        Ok(control) => control,
        Err(message) => return invalid_request(message),
    };
    if let Some(error) = control.http_error {
        return provider_error(error);
    }
    if request.stream {
        let include_usage = request.include_stream_usage();
        return streaming_response(request.model, include_usage, control);
    }

    non_streaming_response(request.model, control)
}

fn non_streaming_response(model: String, control: OpenAiMockControl) -> Response {
    let (message, finish_reason) = match control.common.response_type {
        MockResponseType::Text => (
            json!({
                "role": "assistant",
                "content": MOCK_RESPONSE_TEXT,
                "refusal": null,
                "annotations": []
            }),
            "stop",
        ),
        MockResponseType::ToolUse => (
            json!({
                "role": "assistant",
                "content": null,
                "refusal": null,
                "tool_calls": [{
                    "id": MOCK_TOOL_CALL_ID,
                    "type": "function",
                    "function": {
                        "name": MOCK_TOOL_NAME,
                        "arguments": MOCK_TOOL_ARGUMENTS
                    }
                }]
            }),
            "tool_calls",
        ),
    };

    with_request_id(
        (
            StatusCode::OK,
            Json(json!({
                "id": MOCK_COMPLETION_ID,
                "object": "chat.completion",
                "created": MOCK_CREATED_AT,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": message,
                    "logprobs": null,
                    "finish_reason": finish_reason
                }],
                "usage": usage(&control)
            })),
        )
            .into_response(),
    )
}

fn streaming_response(model: String, include_usage: bool, control: OpenAiMockControl) -> Response {
    let events = streaming_events(&model, include_usage, &control);
    let body = Body::from_stream(DelayedByteStream::new(events, control.common.chunk_delay));
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    with_request_id(response)
}

fn streaming_events(model: &str, include_usage: bool, control: &OpenAiMockControl) -> Vec<Bytes> {
    let mut events = vec![data_event(chunk(
        model,
        json!({ "role": "assistant", "content": "" }),
        None,
        include_usage,
    ))];

    let ended_with_error = match control.common.response_type {
        MockResponseType::Text => append_text_events(&mut events, model, include_usage, control),
        MockResponseType::ToolUse => append_tool_events(&mut events, model, include_usage, control),
    };

    if ended_with_error {
        return events;
    }
    if include_usage {
        events.push(data_event(json!({
            "id": MOCK_COMPLETION_ID,
            "object": "chat.completion.chunk",
            "created": MOCK_CREATED_AT,
            "model": model,
            "choices": [],
            "usage": usage(control)
        })));
    }
    events.push(Bytes::from_static(b"data: [DONE]\n\n"));

    events
}

fn append_text_events(
    events: &mut Vec<Bytes>,
    model: &str,
    include_usage: bool,
    control: &OpenAiMockControl,
) -> bool {
    for (index, content) in split_text(MOCK_RESPONSE_TEXT, control.common.chunk_count)
        .into_iter()
        .enumerate()
    {
        if append_stream_error_if_requested(events, control, index) {
            return true;
        }
        events.push(data_event(chunk(
            model,
            json!({ "content": content }),
            None,
            include_usage,
        )));
    }
    if append_stream_error_if_requested(events, control, control.common.chunk_count) {
        return true;
    }
    events.push(data_event(chunk(
        model,
        json!({}),
        Some("stop"),
        include_usage,
    )));

    false
}

fn append_tool_events(
    events: &mut Vec<Bytes>,
    model: &str,
    include_usage: bool,
    control: &OpenAiMockControl,
) -> bool {
    events[0] = data_event(chunk(
        model,
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "index": 0,
                "id": MOCK_TOOL_CALL_ID,
                "type": "function",
                "function": {
                    "name": MOCK_TOOL_NAME,
                    "arguments": ""
                }
            }]
        }),
        None,
        include_usage,
    ));
    for (index, arguments) in split_text(MOCK_TOOL_ARGUMENTS, control.common.chunk_count)
        .into_iter()
        .enumerate()
    {
        if append_stream_error_if_requested(events, control, index) {
            return true;
        }
        events.push(data_event(chunk(
            model,
            json!({
                "tool_calls": [{
                    "index": 0,
                    "function": {
                        "arguments": arguments
                    }
                }]
            }),
            None,
            include_usage,
        )));
    }
    if append_stream_error_if_requested(events, control, control.common.chunk_count) {
        return true;
    }
    events.push(data_event(chunk(
        model,
        json!({}),
        Some("tool_calls"),
        include_usage,
    )));

    false
}

fn append_stream_error_if_requested(
    events: &mut Vec<Bytes>,
    control: &OpenAiMockControl,
    completed_chunks: usize,
) -> bool {
    if control.common.stream_error_after_chunks != Some(completed_chunks) {
        return false;
    }
    events.push(data_event(json!({
        "error": {
            "message": "Mock OpenAI mid-stream server error.",
            "type": "server_error",
            "param": null,
            "code": null
        }
    })));

    true
}

fn chunk(model: &str, delta: Value, finish_reason: Option<&str>, include_usage: bool) -> Value {
    let mut chunk = json!({
        "id": MOCK_COMPLETION_ID,
        "object": "chat.completion.chunk",
        "created": MOCK_CREATED_AT,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "logprobs": null,
            "finish_reason": finish_reason
        }]
    });
    if include_usage {
        chunk
            .as_object_mut()
            .expect("mock chunk should be a JSON object")
            .insert("usage".to_string(), Value::Null);
    }

    chunk
}

fn usage(control: &OpenAiMockControl) -> Value {
    json!({
        "prompt_tokens": control.prompt_tokens,
        "completion_tokens": control.completion_tokens,
        "total_tokens": control.prompt_tokens + control.completion_tokens,
        "prompt_tokens_details": {
            "cached_tokens": control.cached_prompt_tokens,
            "audio_tokens": 0
        },
        "completion_tokens_details": {
            "reasoning_tokens": 0,
            "audio_tokens": 0,
            "accepted_prediction_tokens": 0,
            "rejected_prediction_tokens": 0
        }
    })
}

fn invalid_request(message: &'static str) -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request_error",
        message,
        None,
    )
}

fn provider_error(error: MockHttpError) -> Response {
    match error {
        MockHttpError::InvalidRequest => invalid_request("Mock OpenAI invalid request error."),
        MockHttpError::Authentication => error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Mock OpenAI authentication error.",
            Some("invalid_api_key"),
        ),
        MockHttpError::Permission => error_response(
            StatusCode::FORBIDDEN,
            "permission_error",
            "Mock OpenAI permission error.",
            None,
        ),
        MockHttpError::NotFound => error_response(
            StatusCode::NOT_FOUND,
            "invalid_request_error",
            "Mock OpenAI resource not found.",
            Some("model_not_found"),
        ),
        MockHttpError::Conflict => error_response(
            StatusCode::CONFLICT,
            "invalid_request_error",
            "Mock OpenAI conflict error.",
            None,
        ),
        MockHttpError::RequestTooLarge => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request_error",
            "Mock OpenAI request too large.",
            None,
        ),
        MockHttpError::UnprocessableEntity => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request_error",
            "Mock OpenAI unprocessable entity.",
            None,
        ),
        MockHttpError::RateLimit => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "Mock OpenAI rate limit error.",
            Some("rate_limit_exceeded"),
        ),
        MockHttpError::Api => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "Mock OpenAI server error.",
            None,
        ),
        MockHttpError::Unavailable => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "server_error",
            "Mock OpenAI service unavailable.",
            None,
        ),
        MockHttpError::Timeout => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "server_error",
            "Mock OpenAI timeout error.",
            None,
        ),
    }
}

fn error_response(
    status: StatusCode,
    error_type: &'static str,
    message: &'static str,
    code: Option<&'static str>,
) -> Response {
    with_request_id(
        (
            status,
            Json(json!({
                "error": {
                    "message": message,
                    "type": error_type,
                    "param": null,
                    "code": code
                }
            })),
        )
            .into_response(),
    )
}

fn with_request_id(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(REQUEST_ID_HEADER, HeaderValue::from_static(MOCK_REQUEST_ID));
    response
}

fn data_event(data: Value) -> Bytes {
    Bytes::from(format!("data: {data}\n\n"))
}
