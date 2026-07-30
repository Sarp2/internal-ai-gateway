use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
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
    content: Option<Value>,
    role: String,
    #[serde(default)]
    tool_calls: Option<Vec<Value>>,
}

impl ChatMessage {
    fn is_valid(&self) -> bool {
        if !matches!(
            self.role.as_str(),
            "developer" | "system" | "user" | "assistant" | "tool"
        ) {
            return false;
        }

        match self.content.as_ref() {
            Some(Value::String(content)) => !content.is_empty(),
            Some(Value::Array(content)) => !content.is_empty(),
            Some(Value::Null) | None => {
                self.role == "assistant"
                    && self
                        .tool_calls
                        .as_ref()
                        .is_some_and(|tool_calls| !tool_calls.is_empty())
            }
            _ => false,
        }
    }
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
    ))];

    let ended_with_error = match control.common.response_type {
        MockResponseType::Text => append_text_events(&mut events, model, control),
        MockResponseType::ToolUse => append_tool_events(&mut events, model, control),
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

fn append_text_events(events: &mut Vec<Bytes>, model: &str, control: &OpenAiMockControl) -> bool {
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
        )));
    }
    if append_stream_error_if_requested(events, control, control.common.chunk_count) {
        return true;
    }
    events.push(data_event(chunk(model, json!({}), Some("stop"))));

    false
}

fn append_tool_events(events: &mut Vec<Bytes>, model: &str, control: &OpenAiMockControl) -> bool {
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
        )));
    }
    if append_stream_error_if_requested(events, control, control.common.chunk_count) {
        return true;
    }
    events.push(data_event(chunk(model, json!({}), Some("tool_calls"))));

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

fn chunk(model: &str, delta: Value, finish_reason: Option<&str>) -> Value {
    json!({
        "id": MOCK_COMPLETION_ID,
        "object": "chat.completion.chunk",
        "created": MOCK_CREATED_AT,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "logprobs": null,
            "finish_reason": finish_reason
        }],
        "usage": null
    })
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
