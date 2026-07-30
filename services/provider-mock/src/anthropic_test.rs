use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderName, HeaderValue};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::time::timeout;
use tower::ServiceExt;

use crate::anthropic::MAX_REQUEST_BODY_BYTES;
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
        response.headers().get("request-id"),
        Some(&HeaderValue::from_static("req_mock_anthropic_001"))
    );
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
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
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
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0,
                    "input_tokens": 12,
                    "output_tokens": 1
                }
            }
        })
    );
    assert_eq!(streamed_text(&events), "Mock Anthropic response.");
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
async fn preserves_text_response_across_configured_chunk_counts() {
    for chunk_count in [1, 2, 4, 25] {
        let chunk_count_header = chunk_count.to_string();
        let response = app()
            .oneshot(messages_request_with_headers(
                valid_messages_body(true),
                &[("x-mock-chunk-count", &chunk_count_header)],
            ))
            .await
            .expect("configured Anthropic text stream should complete");
        let events = sse_events(response_body(response).await);

        assert_eq!(
            events
                .iter()
                .filter(|(event, _)| event == "content_block_delta")
                .count(),
            chunk_count
        );
        assert_eq!(streamed_text(&events), "Mock Anthropic response.");
    }
}

#[tokio::test]
async fn returns_controlled_provider_http_errors() {
    let cases = [
        (
            "400",
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "Mock invalid request error.",
        ),
        (
            "401",
            StatusCode::UNAUTHORIZED,
            "authentication_error",
            "Mock authentication error.",
        ),
        (
            "402",
            StatusCode::PAYMENT_REQUIRED,
            "billing_error",
            "Mock billing error.",
        ),
        (
            "403",
            StatusCode::FORBIDDEN,
            "permission_error",
            "Mock permission error.",
        ),
        (
            "404",
            StatusCode::NOT_FOUND,
            "not_found_error",
            "Mock not found error.",
        ),
        (
            "409",
            StatusCode::CONFLICT,
            "conflict_error",
            "Mock conflict error.",
        ),
        (
            "413",
            StatusCode::PAYLOAD_TOO_LARGE,
            "request_too_large",
            "Mock request too large error.",
        ),
        (
            "429",
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "Mock rate limit error.",
        ),
        (
            "500",
            StatusCode::INTERNAL_SERVER_ERROR,
            "api_error",
            "Mock Anthropic API error.",
        ),
        (
            "504",
            StatusCode::GATEWAY_TIMEOUT,
            "timeout_error",
            "Mock Anthropic timeout error.",
        ),
        (
            "529",
            StatusCode::from_u16(529).expect("529 is a valid HTTP status"),
            "overloaded_error",
            "Mock Anthropic overloaded error.",
        ),
    ];

    for (header, status, error_type, message) in cases {
        let response = app()
            .oneshot(messages_request_with_headers(
                valid_messages_body(false),
                &[("x-mock-http-status", header)],
            ))
            .await
            .expect("controlled Anthropic error request should complete");

        assert_eq!(response.status(), status);
        assert_eq!(
            response_json(response).await,
            json!({
                "type": "error",
                "error": {
                    "type": error_type,
                    "message": message
                },
                "request_id": "req_mock_anthropic_001"
            })
        );
    }
}

#[tokio::test]
async fn returns_prompt_cache_usage_for_json_and_streaming_responses() {
    let headers = [
        ("x-mock-cache-creation-input-tokens", "20"),
        ("x-mock-cache-read-input-tokens", "30"),
    ];
    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(false),
            &headers,
        ))
        .await
        .expect("cached Anthropic mock request should complete");
    let body = response_json(response).await;

    assert_eq!(body["usage"]["cache_creation_input_tokens"], 20);
    assert_eq!(body["usage"]["cache_read_input_tokens"], 30);

    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(true),
            &headers,
        ))
        .await
        .expect("cached streaming Anthropic mock request should complete");
    let events = sse_events(response_body(response).await);

    assert_eq!(
        events[0].1["message"]["usage"]["cache_creation_input_tokens"],
        20
    );
    assert_eq!(
        events[0].1["message"]["usage"]["cache_read_input_tokens"],
        30
    );
}

#[tokio::test]
async fn creates_long_and_slow_streams_from_bounded_controls() {
    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(true),
            &[("x-mock-chunk-count", "5")],
        ))
        .await
        .expect("long Anthropic mock request should complete");
    let events = sse_events(response_body(response).await);

    assert_eq!(
        events
            .iter()
            .filter(|(event, _)| event == "content_block_delta")
            .count(),
        5
    );
    assert_eq!(
        events.last().map(|(event, _)| event.as_str()),
        Some("message_stop")
    );

    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(true),
            &[("x-mock-chunk-delay-ms", "25")],
        ))
        .await
        .expect("slow Anthropic mock request should start");

    assert!(
        timeout(Duration::from_millis(5), response_body(response))
            .await
            .is_err(),
        "delayed stream should not complete before its configured chunk delay"
    );
}

#[tokio::test]
async fn emits_an_error_after_the_configured_stream_chunk() {
    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(true),
            &[
                ("x-mock-chunk-count", "4"),
                ("x-mock-stream-error-after-chunks", "2"),
            ],
        ))
        .await
        .expect("mid-stream Anthropic error request should complete");
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
            "error"
        ]
    );
    assert_eq!(
        events.last().map(|(_, data)| data),
        Some(&json!({
            "type": "error",
            "error": {
                "type": "overloaded_error",
                "message": "Mock mid-stream overloaded error."
            }
        }))
    );
}

#[tokio::test]
async fn returns_anthropic_tool_calls_for_json_and_streaming_responses() {
    let headers = [("x-mock-response-type", "tool_use")];
    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(false),
            &headers,
        ))
        .await
        .expect("Anthropic tool-use mock request should complete");
    let body = response_json(response).await;

    assert_eq!(body["stop_reason"], "tool_use");
    assert_eq!(
        body["content"][0],
        json!({
            "type": "tool_use",
            "id": "toolu_mock_weather_001",
            "name": "get_weather",
            "input": {
                "location": "Istanbul"
            }
        })
    );

    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(true),
            &headers,
        ))
        .await
        .expect("streaming Anthropic tool-use mock request should complete");
    let events = sse_events(response_body(response).await);
    let partial_json = events
        .iter()
        .filter(|(event, _)| event == "content_block_delta")
        .filter_map(|(_, data)| data["delta"]["partial_json"].as_str())
        .collect::<String>();

    assert_eq!(
        events[1].1["content_block"],
        json!({
            "type": "tool_use",
            "id": "toolu_mock_weather_001",
            "name": "get_weather",
            "input": {}
        })
    );
    assert_eq!(
        serde_json::from_str::<Value>(&partial_json).expect("tool input should be valid JSON"),
        json!({ "location": "Istanbul" })
    );
    assert_eq!(events[5].1["delta"]["stop_reason"], "tool_use");
}

#[tokio::test]
async fn rejects_unbounded_mock_controls() {
    let cases = [
        (
            "x-mock-chunk-count",
            "10001",
            "x-mock-chunk-count must be an integer between 1 and 10000",
        ),
        (
            "x-mock-chunk-delay-ms",
            "60001",
            "x-mock-chunk-delay-ms must be a non-negative integer up to 60000",
        ),
        (
            "x-mock-stream-error-after-chunks",
            "3",
            "x-mock-stream-error-after-chunks must not exceed x-mock-chunk-count",
        ),
    ];

    for (name, value, expected_message) in cases {
        let response = app()
            .oneshot(messages_request_with_headers(
                valid_messages_body(true),
                &[(name, value)],
            ))
            .await
            .expect("invalid mock control request should complete");

        assert_invalid_request(response, expected_message).await;
    }

    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(true),
            &[
                ("x-mock-chunk-count", "10000"),
                ("x-mock-chunk-delay-ms", "60000"),
            ],
        ))
        .await
        .expect("multi-day mock stream request should complete");

    assert_invalid_request(response, "mock stream duration must not exceed one hour").await;
}

#[tokio::test]
async fn accepts_a_mock_stream_with_an_exact_one_hour_duration() {
    let response = app()
        .oneshot(messages_request_with_headers(
            valid_messages_body(true),
            &[
                ("x-mock-chunk-count", "3595"),
                ("x-mock-chunk-delay-ms", "1000"),
            ],
        ))
        .await
        .expect("one-hour mock stream request should start");

    assert_eq!(response.status(), StatusCode::OK);
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

#[tokio::test]
async fn accepts_request_body_at_anthropic_messages_limit() {
    let response = app()
        .oneshot(raw_messages_request(&messages_body_with_size(
            MAX_REQUEST_BODY_BYTES,
        )))
        .await
        .expect("maximum-size Anthropic request should complete");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn rejects_request_body_above_anthropic_messages_limit() {
    let response = app()
        .oneshot(raw_messages_request(&messages_body_with_size(
            MAX_REQUEST_BODY_BYTES + 1,
        )))
        .await
        .expect("oversized Anthropic request should complete");

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

fn messages_request(body: Value) -> Request<Body> {
    messages_request_with_headers(body, &[])
}

fn messages_request_with_headers(body: Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(
            HeaderName::from_bytes(name.as_bytes()).expect("mock header name should be valid"),
            HeaderValue::from_str(value).expect("mock header value should be valid"),
        );
    }

    request
        .body(Body::from(body.to_string()))
        .expect("Anthropic request should be valid")
}

fn raw_messages_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("Anthropic request should be valid")
}

fn valid_messages_body(stream: bool) -> Value {
    json!({
        "model": "claude-test-model",
        "max_tokens": 128,
        "messages": [
            {
                "role": "user",
                "content": "Hello"
            }
        ],
        "stream": stream
    })
}

fn messages_body_with_size(size: usize) -> String {
    const PREFIX: &str =
        r#"{"model":"claude-test-model","max_tokens":128,"messages":[{"role":"user","content":""#;
    const SUFFIX: &str = r#""}]}"#;
    let content_size = size
        .checked_sub(PREFIX.len() + SUFFIX.len())
        .expect("requested body size should fit the valid JSON wrapper");

    format!("{PREFIX}{}{SUFFIX}", "a".repeat(content_size))
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
            },
            "request_id": "req_mock_anthropic_001"
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

fn streamed_text(events: &[(String, Value)]) -> String {
    events
        .iter()
        .filter(|(event, _)| event == "content_block_delta")
        .filter_map(|(_, data)| data["delta"]["text"].as_str())
        .collect()
}
