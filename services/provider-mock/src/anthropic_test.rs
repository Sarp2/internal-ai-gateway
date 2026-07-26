use axum::body::{Body, Bytes, to_bytes};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::app::app;

#[tokio::test]
async fn returns_a_deterministic_non_streaming_message() {
    let response = app()
        .oneshot(messages_request(json!({
            "model": "claude-test-model",
            "max_tokens": 128,
            "messages": [
                {
                    "role": "user",
                    "content": "Hello"
                }
            ]
        })))
        .await
        .expect("Anthropic mock request should succeed");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response_json(response).await,
        json!({
            "id": "msg_mock_anthropic_001",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": "Mock Anthropic response."
                }
            ],
            "model": "claude-test-model",
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 12,
                "output_tokens": 6
            }
        })
    );
}

#[tokio::test]
async fn returns_anthropic_compatible_streaming_events() {
    let response = app()
        .oneshot(messages_request(json!({
            "model": "claude-test-model",
            "max_tokens": 128,
            "messages": [
                {
                    "role": "user",
                    "content": "Hello"
                }
            ],
            "stream": true
        })))
        .await
        .expect("Anthropic mock request should complete");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&"text/event-stream".parse().expect("content type is valid"))
    );
    assert_eq!(
        response.headers().get(CACHE_CONTROL),
        Some(&"no-cache".parse().expect("cache control is valid"))
    );

    let events = sse_events(response_body(response).await);
    assert_eq!(
        events
            .iter()
            .map(|(event, _)| event.as_str())
            .collect::<Vec<_>>(),
        [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop"
        ]
    );
    assert_eq!(
        events[0].1,
        json!({
            "type": "message_start",
            "message": {
                "id": "msg_mock_anthropic_001",
                "type": "message",
                "role": "assistant",
                "content": [],
                "model": "claude-test-model",
                "stop_reason": null,
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 1
                }
            }
        })
    );
    assert_eq!(events[2].1["delta"]["text"], "Mock Anthropic ");
    assert_eq!(events[3].1["delta"]["text"], "response.");
    assert_eq!(
        events[5].1,
        json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": "end_turn",
                "stop_sequence": null
            },
            "usage": {
                "output_tokens": 6
            }
        })
    );
    assert_eq!(events[6].1, json!({ "type": "message_stop" }));
}

#[tokio::test]
async fn rejects_invalid_required_message_fields() {
    let cases = [
        (
            json!({
                "model": " ",
                "max_tokens": 128,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
            "model must not be empty",
        ),
        (
            json!({
                "model": "claude-test-model",
                "max_tokens": 0,
                "messages": [{"role": "user", "content": "Hello"}]
            }),
            "max_tokens must be greater than zero",
        ),
        (
            json!({
                "model": "claude-test-model",
                "max_tokens": 128,
                "messages": []
            }),
            "messages must not be empty",
        ),
        (
            json!({
                "model": "claude-test-model",
                "max_tokens": 128,
                "messages": [{"role": "system", "content": "Hello"}]
            }),
            "request body must contain valid Anthropic Messages JSON",
        ),
        (
            json!({
                "model": "claude-test-model",
                "max_tokens": 128,
                "messages": [{"role": "user", "content": []}]
            }),
            "each message must have a valid role and non-empty content",
        ),
    ];

    for (request, expected_message) in cases {
        let response = app()
            .oneshot(messages_request(request))
            .await
            .expect("invalid Anthropic mock request should complete");

        assert_invalid_request(response, expected_message).await;
    }
}

#[tokio::test]
async fn rejects_duplicate_known_request_fields() {
    let response = app()
        .oneshot(raw_messages_request(
            r#"{
                "model": "claude-test-model",
                "model": "claude-other-model",
                "max_tokens": 128,
                "messages": [{"role": "user", "content": "Hello"}]
            }"#,
        ))
        .await
        .expect("duplicate Anthropic request should complete");

    assert_invalid_request(
        response,
        "request body must contain valid Anthropic Messages JSON",
    )
    .await;
}

fn messages_request(body: Value) -> Request<Body> {
    raw_messages_request(&body.to_string())
}

fn raw_messages_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("Anthropic request should be valid")
}

async fn assert_invalid_request(response: axum::response::Response, expected_message: &str) {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(response).await,
        json!({
            "type": "error",
            "error": {
                "type": "invalid_request_error",
                "message": expected_message
            }
        })
    );
}

async fn response_json(response: axum::response::Response) -> Value {
    let body = response_body(response).await;

    serde_json::from_slice(&body).expect("Anthropic response should contain valid JSON")
}

async fn response_body(response: axum::response::Response) -> Bytes {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("Anthropic response body should be readable")
}

fn sse_events(body: Bytes) -> Vec<(String, Value)> {
    let body = String::from_utf8(body.to_vec()).expect("SSE response should contain UTF-8");

    body.trim_end()
        .split("\n\n")
        .map(|event| {
            let mut lines = event.lines();
            let event_name = lines
                .next()
                .and_then(|line| line.strip_prefix("event: "))
                .expect("SSE event should have an event field");
            let data = lines
                .next()
                .and_then(|line| line.strip_prefix("data: "))
                .expect("SSE event should have a data field");

            (
                event_name.to_string(),
                serde_json::from_str(data).expect("SSE data should contain valid JSON"),
            )
        })
        .collect()
}
