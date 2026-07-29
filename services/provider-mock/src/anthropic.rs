use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::time::{Sleep, sleep};

use crate::anthropic_control::{AnthropicMockControl, MockHttpError, MockResponseType};

const MOCK_MESSAGE_ID: &str = "msg_mock_anthropic_001";
const MOCK_REQUEST_ID: &str = "req_mock_anthropic_001";
const MOCK_RESPONSE_TEXT: &str = "Mock Anthropic response.";
const MOCK_INPUT_TOKENS: u64 = 12;
const MOCK_OUTPUT_TOKENS: u64 = 6;
const MOCK_TOOL_ID: &str = "toolu_mock_weather_001";
const MOCK_TOOL_NAME: &str = "get_weather";
const MOCK_TOOL_INPUT: &str = r#"{"location":"Istanbul"}"#;
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("request-id");

#[derive(Deserialize)]
pub(crate) struct MessagesRequest {
    max_tokens: u64,
    messages: Vec<Message>,
    model: String,
    #[serde(default)]
    stream: bool,
}

impl MessagesRequest {
    fn validate(&self) -> Result<(), &'static str> {
        if self.model.trim().is_empty() {
            return Err("model must not be empty");
        }
        if self.max_tokens == 0 {
            return Err("max_tokens must be greater than zero");
        }
        if self.messages.is_empty() {
            return Err("messages must not be empty");
        }
        if self.messages.iter().any(|message| !message.is_valid()) {
            return Err("each message must have a valid role and non-empty content");
        }

        Ok(())
    }
}

#[derive(Deserialize)]
struct Message {
    content: MessageContent,
    role: MessageRole,
}

impl Message {
    fn is_valid(&self) -> bool {
        matches!(self.role, MessageRole::User | MessageRole::Assistant)
            && match &self.content {
                MessageContent::Text(text) => !text.is_empty(),
                MessageContent::Blocks(blocks) => {
                    !blocks.is_empty()
                        && blocks
                            .iter()
                            .all(|block| !block.content_type.trim().is_empty())
                }
            }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    content_type: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum MessageRole {
    Assistant,
    User,
}

#[derive(Serialize)]
pub(crate) struct MessageResponse {
    id: &'static str,
    r#type: &'static str,
    role: &'static str,
    content: Vec<Value>,
    model: String,
    stop_reason: &'static str,
    stop_sequence: Option<&'static str>,
    usage: Usage,
}

#[derive(Serialize)]
struct Usage {
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Serialize)]
struct ErrorResponse {
    r#type: &'static str,
    error: ErrorDetail,
    request_id: &'static str,
}

#[derive(Serialize)]
struct ErrorDetail {
    r#type: &'static str,
    message: &'static str,
}

pub(crate) async fn messages(
    headers: HeaderMap,
    payload: Result<Json<MessagesRequest>, JsonRejection>,
) -> Response {
    let Json(request) = match payload {
        Ok(request) => request,
        Err(_) => {
            return invalid_request("request body must contain valid Anthropic Messages JSON");
        }
    };
    if let Err(message) = request.validate() {
        return invalid_request(message);
    }
    let control = match AnthropicMockControl::from_headers(&headers) {
        Ok(control) => control,
        Err(message) => return invalid_request(message),
    };
    if let Some(error) = control.http_error {
        return provider_error(error);
    }
    if request.stream {
        return streaming_response(request.model, control);
    }

    let (content, stop_reason) = match control.response_type {
        MockResponseType::Text => (
            vec![json!({
                "type": "text",
                "text": MOCK_RESPONSE_TEXT
            })],
            "end_turn",
        ),
        MockResponseType::ToolUse => (
            vec![json!({
                "type": "tool_use",
                "id": MOCK_TOOL_ID,
                "name": MOCK_TOOL_NAME,
                "input": {
                    "location": "Istanbul"
                }
            })],
            "tool_use",
        ),
    };
    with_request_id(
        (
            StatusCode::OK,
            Json(MessageResponse {
                id: MOCK_MESSAGE_ID,
                r#type: "message",
                role: "assistant",
                content,
                model: request.model,
                stop_reason,
                stop_sequence: None,
                usage: Usage {
                    cache_creation_input_tokens: control.cache_creation_input_tokens,
                    cache_read_input_tokens: control.cache_read_input_tokens,
                    input_tokens: MOCK_INPUT_TOKENS,
                    output_tokens: MOCK_OUTPUT_TOKENS,
                },
            }),
        )
            .into_response(),
    )
}

fn invalid_request(message: &'static str) -> Response {
    error_response(StatusCode::BAD_REQUEST, "invalid_request_error", message)
}

fn provider_error(error: MockHttpError) -> Response {
    match error {
        MockHttpError::InvalidRequest => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "Mock invalid request error.",
        ),
        MockHttpError::Authentication => error_response(
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Mock authentication error.",
        ),
        MockHttpError::Billing => error_response(
            StatusCode::PAYMENT_REQUIRED,
            "billing_error",
            "Mock billing error.",
        ),
        MockHttpError::Permission => error_response(
            StatusCode::FORBIDDEN,
            "permission_error",
            "Mock permission error.",
        ),
        MockHttpError::NotFound => error_response(
            StatusCode::NOT_FOUND,
            "not_found_error",
            "Mock not found error.",
        ),
        MockHttpError::Conflict => error_response(
            StatusCode::CONFLICT,
            "conflict_error",
            "Mock conflict error.",
        ),
        MockHttpError::RequestTooLarge => error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            "Mock request too large error.",
        ),
        MockHttpError::RateLimit => error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "Mock rate limit error.",
        ),
        MockHttpError::Api => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "api_error",
            "Mock Anthropic API error.",
        ),
        MockHttpError::Timeout => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout_error",
            "Mock Anthropic timeout error.",
        ),
        MockHttpError::Overloaded => error_response(
            StatusCode::from_u16(529).expect("529 is a valid HTTP status"),
            "overloaded_error",
            "Mock Anthropic overloaded error.",
        ),
    }
}

fn error_response(status: StatusCode, error_type: &'static str, message: &'static str) -> Response {
    with_request_id(
        (
            status,
            Json(ErrorResponse {
                r#type: "error",
                error: ErrorDetail {
                    r#type: error_type,
                    message,
                },
                request_id: MOCK_REQUEST_ID,
            }),
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

fn streaming_response(model: String, control: AnthropicMockControl) -> Response {
    let events = streaming_events(&model, &control);
    let body = Body::from_stream(AnthropicEventStream {
        delay: control.chunk_delay,
        events: events.into_iter(),
        sleep: None,
    });
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    with_request_id(response)
}

fn streaming_events(model: &str, control: &AnthropicMockControl) -> Vec<Bytes> {
    let mut events = vec![sse_event(
        "message_start",
        json!({
            "type": "message_start",
            "message": {
                "id": MOCK_MESSAGE_ID,
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": model,
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "cache_creation_input_tokens": control.cache_creation_input_tokens,
                    "cache_read_input_tokens": control.cache_read_input_tokens,
                    "input_tokens": MOCK_INPUT_TOKENS,
                    "output_tokens": 1
                }
            }
        }),
    )];

    match control.response_type {
        MockResponseType::Text => append_text_events(&mut events, control),
        MockResponseType::ToolUse => append_tool_events(&mut events, control),
    }

    events
}

fn append_text_events(events: &mut Vec<Bytes>, control: &AnthropicMockControl) {
    events.push(sse_event(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "text",
                "text": ""
            }
        }),
    ));

    for index in 0..control.chunk_count {
        if append_stream_error_if_requested(events, control, index) {
            return;
        }
        events.push(sse_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": mock_text_chunk(index, control.chunk_count)
                }
            }),
        ));
    }
    if append_stream_error_if_requested(events, control, control.chunk_count) {
        return;
    }
    append_stream_completion(events, "end_turn");
}

fn append_tool_events(events: &mut Vec<Bytes>, control: &AnthropicMockControl) {
    events.push(sse_event(
        "content_block_start",
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {
                "type": "tool_use",
                "id": MOCK_TOOL_ID,
                "name": MOCK_TOOL_NAME,
                "input": {}
            }
        }),
    ));

    for (index, partial_json) in split_tool_input(control.chunk_count)
        .into_iter()
        .enumerate()
    {
        if append_stream_error_if_requested(events, control, index) {
            return;
        }
        events.push(sse_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "input_json_delta",
                    "partial_json": partial_json
                }
            }),
        ));
    }
    if append_stream_error_if_requested(events, control, control.chunk_count) {
        return;
    }
    append_stream_completion(events, "tool_use");
}

fn append_stream_error_if_requested(
    events: &mut Vec<Bytes>,
    control: &AnthropicMockControl,
    completed_chunks: usize,
) -> bool {
    if control.stream_error_after_chunks != Some(completed_chunks) {
        return false;
    }
    events.push(sse_event(
        "error",
        json!({
            "type": "error",
            "error": {
                "type": "overloaded_error",
                "message": "Mock mid-stream overloaded error."
            }
        }),
    ));

    true
}

fn append_stream_completion(events: &mut Vec<Bytes>, stop_reason: &str) {
    events.extend([
        sse_event(
            "content_block_stop",
            json!({
                "type": "content_block_stop",
                "index": 0
            }),
        ),
        sse_event(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": stop_reason,
                    "stop_sequence": null
                },
                "usage": {
                    "output_tokens": MOCK_OUTPUT_TOKENS
                }
            }),
        ),
        sse_event(
            "message_stop",
            json!({
                "type": "message_stop"
            }),
        ),
    ]);
}

fn mock_text_chunk(index: usize, chunk_count: usize) -> String {
    if chunk_count == 2 {
        return ["Mock Anthropic ", "response."][index].to_string();
    }

    format!("mock-chunk-{} ", index + 1)
}

fn split_tool_input(chunk_count: usize) -> Vec<&'static str> {
    let chunk_size = MOCK_TOOL_INPUT.len().div_ceil(chunk_count);
    let mut chunks = MOCK_TOOL_INPUT
        .as_bytes()
        .chunks(chunk_size)
        .map(|chunk| std::str::from_utf8(chunk).expect("mock tool input is ASCII"))
        .collect::<Vec<_>>();
    chunks.resize(chunk_count, "");

    chunks
}

fn sse_event(event: &str, data: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

struct AnthropicEventStream {
    delay: Duration,
    events: std::vec::IntoIter<Bytes>,
    sleep: Option<Pin<Box<Sleep>>>,
}

impl Stream for AnthropicEventStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.events.len() == 0 {
            return Poll::Ready(None);
        }
        if self.delay.is_zero() {
            return Poll::Ready(self.events.next().map(Ok));
        }

        if self.sleep.is_none() {
            self.sleep = Some(Box::pin(sleep(self.delay)));
        }
        let sleep = self
            .sleep
            .as_mut()
            .expect("stream delay should exist before polling");
        if sleep.as_mut().poll(context).is_pending() {
            return Poll::Pending;
        }
        self.sleep = None;

        Poll::Ready(self.events.next().map(Ok))
    }
}
