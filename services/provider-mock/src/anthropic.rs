use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MOCK_MESSAGE_ID: &str = "msg_mock_anthropic_001";
const MOCK_RESPONSE_TEXT: &str = "Mock Anthropic response.";
const MOCK_INPUT_TOKENS: u64 = 12;
const MOCK_OUTPUT_TOKENS: u64 = 6;

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
    content: [TextContent; 1],
    model: String,
    stop_reason: &'static str,
    stop_sequence: Option<&'static str>,
    usage: Usage,
}

#[derive(Serialize)]
struct TextContent {
    r#type: &'static str,
    text: &'static str,
}

#[derive(Serialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Serialize)]
struct ErrorResponse {
    r#type: &'static str,
    error: ErrorDetail,
}

#[derive(Serialize)]
struct ErrorDetail {
    r#type: &'static str,
    message: &'static str,
}

pub(crate) async fn messages(payload: Result<Json<MessagesRequest>, JsonRejection>) -> Response {
    let Json(request) = match payload {
        Ok(request) => request,
        Err(_) => {
            return invalid_request("request body must contain valid Anthropic Messages JSON");
        }
    };
    if let Err(message) = request.validate() {
        return invalid_request(message);
    }
    if request.stream {
        return streaming_response(request.model);
    }

    (
        StatusCode::OK,
        Json(MessageResponse {
            id: MOCK_MESSAGE_ID,
            r#type: "message",
            role: "assistant",
            content: [TextContent {
                r#type: "text",
                text: MOCK_RESPONSE_TEXT,
            }],
            model: request.model,
            stop_reason: "end_turn",
            stop_sequence: None,
            usage: Usage {
                input_tokens: MOCK_INPUT_TOKENS,
                output_tokens: MOCK_OUTPUT_TOKENS,
            },
        }),
    )
        .into_response()
}

fn invalid_request(message: &'static str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            r#type: "error",
            error: ErrorDetail {
                r#type: "invalid_request_error",
                message,
            },
        }),
    )
        .into_response()
}

fn streaming_response(model: String) -> Response {
    let events = [
        sse_event(
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
                        "input_tokens": MOCK_INPUT_TOKENS,
                        "output_tokens": 1
                    }
                }
            }),
        ),
        sse_event(
            "content_block_start",
            json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {
                    "type": "text",
                    "text": ""
                }
            }),
        ),
        sse_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": "Mock Anthropic "
                }
            }),
        ),
        sse_event(
            "content_block_delta",
            json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": "response."
                }
            }),
        ),
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
                    "stop_reason": "end_turn",
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
    ];
    let body = Body::from_stream(AnthropicEventStream {
        events: events.into_iter(),
    });
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));

    response
}

fn sse_event(event: &str, data: Value) -> Bytes {
    Bytes::from(format!("event: {event}\ndata: {data}\n\n"))
}

struct AnthropicEventStream {
    events: std::array::IntoIter<Bytes, 7>,
}

impl Stream for AnthropicEventStream {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(self.events.next().map(Ok))
    }
}
