use std::time::Duration;

use axum::body::{Body, Bytes, to_bytes};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderName, HeaderValue};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tokio::time::timeout;
use tower::ServiceExt;

use crate::app::app;

#[tokio::test]
async fn returns_a_deterministic_non_streaming_chat_completion() {
    let response = app()
        .oneshot(chat_completions_request(valid_request(false)))
        .await
        .expect("OpenAI mock request should complete");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("x-request-id"),
        Some(&HeaderValue::from_static("req_mock_openai_001"))
    );
    assert_eq!(
        response_json(response).await,
        json!({
            "id": "chatcmpl_mock_openai_001",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-test-model",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Mock OpenAI response.",
                    "refusal": null,
                    "annotations": []
                },
                "logprobs": null,
                "finish_reason": "stop"
            }],
            "usage": expected_usage(12, 6, 0)
        })
    );
}

#[tokio::test]
async fn returns_openai_compatible_streaming_events_with_usage() {
    let mut request = valid_request(true);
    request["stream_options"] = json!({ "include_usage": true });
    let response = app()
        .oneshot(chat_completions_request(request))
        .await
        .expect("streaming OpenAI mock request should complete");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE),
        Some(&HeaderValue::from_static("text/event-stream"))
    );
    assert_eq!(
        response.headers().get(CACHE_CONTROL),
        Some(&HeaderValue::from_static("no-cache"))
    );

    let events = data_events(response_body(response).await);
    assert_eq!(events.len(), 6);
    assert_eq!(events[0]["choices"][0]["delta"]["role"], "assistant");
    assert_eq!(streamed_text(&events), "Mock OpenAI response.");
    assert_eq!(events[3]["choices"][0]["finish_reason"], "stop");
    assert_eq!(events[4]["choices"], json!([]));
    assert_eq!(events[4]["usage"], expected_usage(12, 6, 0));
    assert_eq!(events[5], Value::String("[DONE]".to_string()));
}

#[tokio::test]
async fn omits_usage_chunk_when_stream_options_do_not_request_it() {
    let response = app()
        .oneshot(chat_completions_request(valid_request(true)))
        .await
        .expect("streaming OpenAI mock request should complete");
    let events = data_events(response_body(response).await);

    assert_eq!(events.len(), 5);
    assert_eq!(events[4], Value::String("[DONE]".to_string()));
    assert!(
        events
            .iter()
            .filter(|event| !event.is_string())
            .all(|event| !event
                .as_object()
                .is_some_and(|event| event.contains_key("usage")))
    );
}

#[tokio::test]
async fn returns_configured_openai_usage() {
    let response = app()
        .oneshot(chat_completions_request_with_headers(
            valid_request(false),
            &[
                ("x-mock-prompt-tokens", "20"),
                ("x-mock-completion-tokens", "30"),
                ("x-mock-cached-prompt-tokens", "10"),
            ],
        ))
        .await
        .expect("configured OpenAI usage request should complete");

    assert_eq!(
        response_json(response).await["usage"],
        expected_usage(20, 30, 10)
    );
}

#[tokio::test]
async fn returns_controlled_openai_http_errors() {
    let cases = [
        ("400", StatusCode::BAD_REQUEST, "invalid_request_error"),
        ("401", StatusCode::UNAUTHORIZED, "authentication_error"),
        ("403", StatusCode::FORBIDDEN, "permission_error"),
        ("404", StatusCode::NOT_FOUND, "invalid_request_error"),
        ("409", StatusCode::CONFLICT, "invalid_request_error"),
        (
            "413",
            StatusCode::PAYLOAD_TOO_LARGE,
            "invalid_request_error",
        ),
        (
            "422",
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request_error",
        ),
        ("429", StatusCode::TOO_MANY_REQUESTS, "rate_limit_error"),
        ("500", StatusCode::INTERNAL_SERVER_ERROR, "server_error"),
        ("503", StatusCode::SERVICE_UNAVAILABLE, "server_error"),
        ("504", StatusCode::GATEWAY_TIMEOUT, "server_error"),
    ];

    for (status, expected_status, expected_type) in cases {
        let response = app()
            .oneshot(chat_completions_request_with_headers(
                valid_request(false),
                &[("x-mock-http-status", status)],
            ))
            .await
            .expect("OpenAI error request should complete");

        assert_eq!(response.status(), expected_status);
        assert_eq!(
            response.headers().get("x-request-id"),
            Some(&HeaderValue::from_static("req_mock_openai_001"))
        );
        assert_eq!(
            response_json(response).await["error"]["type"],
            expected_type
        );
    }
}

#[tokio::test]
async fn emits_an_openai_error_after_configured_stream_chunks() {
    let response = app()
        .oneshot(chat_completions_request_with_headers(
            request_with_usage_stream(),
            &[
                ("x-mock-chunk-count", "4"),
                ("x-mock-stream-error-after-chunks", "2"),
            ],
        ))
        .await
        .expect("mid-stream OpenAI error request should complete");
    let events = data_events(response_body(response).await);

    assert_eq!(events.len(), 4);
    assert_eq!(events[3]["error"]["type"], "server_error");
    assert!(
        events
            .iter()
            .all(|event| event != &Value::String("[DONE]".to_string()))
    );
}

#[tokio::test]
async fn returns_openai_tool_calls_for_json_and_streaming_responses() {
    let response = app()
        .oneshot(chat_completions_request_with_headers(
            valid_request(false),
            &[("x-mock-response-type", "tool_use")],
        ))
        .await
        .expect("OpenAI tool-call mock request should complete");
    let body = response_json(response).await;

    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        body["choices"][0]["message"]["tool_calls"][0],
        json!({
            "id": "call_mock_weather_001",
            "type": "function",
            "function": {
                "name": "get_weather",
                "arguments": "{\"location\":\"Istanbul\"}"
            }
        })
    );

    let response = app()
        .oneshot(chat_completions_request_with_headers(
            request_with_usage_stream(),
            &[
                ("x-mock-response-type", "tool_use"),
                ("x-mock-chunk-count", "4"),
            ],
        ))
        .await
        .expect("streaming OpenAI tool-call request should complete");
    let events = data_events(response_body(response).await);
    let arguments = events
        .iter()
        .filter_map(|event| {
            event["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
        })
        .collect::<String>();

    assert_eq!(
        events[0]["choices"][0]["delta"]["tool_calls"][0]["id"],
        "call_mock_weather_001"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&arguments).expect("tool arguments should be valid JSON"),
        json!({ "location": "Istanbul" })
    );
    assert_eq!(events[5]["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(events[6]["usage"], expected_usage(12, 6, 0));
    assert_eq!(events[7], Value::String("[DONE]".to_string()));
}

#[tokio::test]
async fn preserves_openai_text_across_configured_chunk_counts() {
    for chunk_count in [1, 2, 4, 25] {
        let chunk_count_header = chunk_count.to_string();
        let response = app()
            .oneshot(chat_completions_request_with_headers(
                request_with_usage_stream(),
                &[("x-mock-chunk-count", &chunk_count_header)],
            ))
            .await
            .expect("configured OpenAI stream should complete");
        let events = data_events(response_body(response).await);

        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event["choices"][0]["delta"]["role"].is_null()
                        && event["choices"][0]["delta"]["content"].is_string()
                })
                .count(),
            chunk_count
        );
        assert_eq!(streamed_text(&events), "Mock OpenAI response.");
    }
}

#[tokio::test]
async fn supports_bounded_slow_and_long_openai_streams() {
    let response = app()
        .oneshot(chat_completions_request_with_headers(
            request_with_usage_stream(),
            &[("x-mock-chunk-count", "5")],
        ))
        .await
        .expect("long OpenAI mock stream should complete");
    let events = data_events(response_body(response).await);

    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event["choices"][0]["delta"]["role"].is_null()
                    && event["choices"][0]["delta"]["content"].is_string()
            })
            .count(),
        5
    );

    let response = app()
        .oneshot(chat_completions_request_with_headers(
            valid_request(true),
            &[("x-mock-chunk-delay-ms", "25")],
        ))
        .await
        .expect("slow OpenAI mock stream should start");

    assert!(
        timeout(Duration::from_millis(5), response_body(response))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rejects_invalid_openai_requests_and_controls() {
    let cases = [
        (
            json!({
                "model": "",
                "messages": [{"role": "user", "content": "Hello"}]
            }),
            "model must not be empty",
        ),
        (
            json!({
                "model": "gpt-test-model",
                "messages": []
            }),
            "messages must not be empty",
        ),
        (
            json!({
                "model": "gpt-test-model",
                "messages": [{"role": "unknown", "content": "Hello"}]
            }),
            "each message must have a valid role and content",
        ),
        (
            json!({
                "model": "gpt-test-model",
                "messages": [{"role": "user", "content": "Hello"}],
                "max_completion_tokens": 0
            }),
            "max_completion_tokens must be greater than zero",
        ),
    ];

    for (request, message) in cases {
        let response = app()
            .oneshot(chat_completions_request(request))
            .await
            .expect("invalid OpenAI request should complete");

        assert_openai_invalid_request(response, message).await;
    }

    let response = app()
        .oneshot(chat_completions_request_with_headers(
            valid_request(false),
            &[
                ("x-mock-prompt-tokens", "10"),
                ("x-mock-cached-prompt-tokens", "11"),
            ],
        ))
        .await
        .expect("invalid OpenAI usage control should complete");

    assert_openai_invalid_request(
        response,
        "x-mock-cached-prompt-tokens must not exceed x-mock-prompt-tokens",
    )
    .await;
}

#[tokio::test]
async fn accepts_valid_openai_multipart_content() {
    let requests = [
        json!({
            "model": "gpt-test-model",
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": "Describe these inputs."
                    },
                    {
                        "type": "image_url",
                        "image_url": {
                            "url": "https://example.com/image.png",
                            "detail": "high"
                        }
                    },
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": "bW9jay1hdWRpbw==",
                            "format": "wav"
                        }
                    },
                    {
                        "type": "file",
                        "file": {
                            "file_id": "file_mock_001"
                        }
                    },
                    {
                        "type": "file",
                        "file": {
                            "file_data": "bW9jay1maWxl",
                            "filename": "notes.txt"
                        }
                    }
                ]
            }]
        }),
        json!({
            "model": "gpt-test-model",
            "messages": [{
                "role": "assistant",
                "content": [{
                    "type": "refusal",
                    "refusal": "Mock refusal."
                }]
            }]
        }),
    ];

    for request in requests {
        let response = app()
            .oneshot(chat_completions_request(request))
            .await
            .expect("valid multipart OpenAI request should complete");

        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn rejects_malformed_openai_multipart_content() {
    let malformed_shapes = [
        json!([123]),
        json!([{}]),
        json!([{
            "type": "unknown",
            "value": "invalid"
        }]),
        json!([{
            "type": "text"
        }]),
        json!([{
            "type": "image_url",
            "image_url": {}
        }]),
    ];

    for content in malformed_shapes {
        let response = app()
            .oneshot(chat_completions_request(json!({
                "model": "gpt-test-model",
                "messages": [{
                    "role": "user",
                    "content": content
                }]
            })))
            .await
            .expect("malformed multipart OpenAI request should complete");

        assert_openai_invalid_request(
            response,
            "request body must contain valid Chat Completions JSON",
        )
        .await;
    }

    let invalid_values = [
        json!([{
            "type": "text",
            "text": ""
        }]),
        json!([{
            "type": "image_url",
            "image_url": {
                "url": "",
                "detail": "high"
            }
        }]),
        json!([{
            "type": "input_audio",
            "input_audio": {
                "data": "bW9jay1hdWRpbw==",
                "format": "flac"
            }
        }]),
        json!([{
            "type": "file",
            "file": {
                "file_data": "bW9jay1maWxl"
            }
        }]),
        json!([{
            "type": "refusal",
            "refusal": "Not valid for a user message."
        }]),
    ];

    for content in invalid_values {
        let response = app()
            .oneshot(chat_completions_request(json!({
                "model": "gpt-test-model",
                "messages": [{
                    "role": "user",
                    "content": content
                }]
            })))
            .await
            .expect("invalid multipart OpenAI request should complete");

        assert_openai_invalid_request(response, "each message must have a valid role and content")
            .await;
    }
}

#[tokio::test]
async fn accepts_valid_assistant_tool_call_messages() {
    let response = app()
        .oneshot(chat_completions_request(json!({
            "model": "gpt-test-model",
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_123",
                    "type": "function",
                    "function": {
                        "name": "get_weather",
                        "arguments": "{\"location\":\"Istanbul\"}"
                    }
                }]
            }]
        })))
        .await
        .expect("valid assistant tool-call request should complete");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn rejects_malformed_assistant_tool_call_messages() {
    let malformed_tool_calls = [
        json!([123]),
        json!([{}]),
        json!([{
            "id": "call_123",
            "type": "custom",
            "function": {
                "name": "get_weather",
                "arguments": "{}"
            }
        }]),
        json!([{
            "id": "call_123",
            "type": "function",
            "function": {
                "name": "get_weather",
                "arguments": 123
            }
        }]),
    ];

    for tool_calls in malformed_tool_calls {
        let response = app()
            .oneshot(chat_completions_request(json!({
                "model": "gpt-test-model",
                "messages": [{
                    "role": "assistant",
                    "content": null,
                    "tool_calls": tool_calls
                }]
            })))
            .await
            .expect("malformed assistant tool-call request should complete");

        assert_openai_invalid_request(
            response,
            "request body must contain valid Chat Completions JSON",
        )
        .await;
    }

    let invalid_tool_calls = [
        json!([]),
        json!([{
            "id": "",
            "type": "function",
            "function": {
                "name": "get_weather",
                "arguments": "{}"
            }
        }]),
        json!([{
            "id": "call_123",
            "type": "function",
            "function": {
                "name": "",
                "arguments": "{}"
            }
        }]),
        json!([{
            "id": "call_123",
            "type": "function",
            "function": {
                "name": "get_weather",
                "arguments": ""
            }
        }]),
    ];

    for tool_calls in invalid_tool_calls {
        let response = app()
            .oneshot(chat_completions_request(json!({
                "model": "gpt-test-model",
                "messages": [{
                    "role": "assistant",
                    "content": null,
                    "tool_calls": tool_calls
                }]
            })))
            .await
            .expect("invalid assistant tool-call request should complete");

        assert_openai_invalid_request(response, "each message must have a valid role and content")
            .await;
    }
}

#[tokio::test]
async fn rejects_duplicate_openai_control_fields() {
    let response = app()
        .oneshot(raw_chat_completions_request(
            r#"{
                "model": "gpt-test-model",
                "messages": [{"role": "user", "content": "Hello"}],
                "stream": true,
                "stream": false
            }"#,
        ))
        .await
        .expect("duplicate OpenAI request should complete");

    assert_openai_invalid_request(
        response,
        "request body must contain valid Chat Completions JSON",
    )
    .await;
}

fn valid_request(stream: bool) -> Value {
    json!({
        "model": "gpt-test-model",
        "messages": [{
            "role": "user",
            "content": "Hello"
        }],
        "stream": stream
    })
}

fn request_with_usage_stream() -> Value {
    let mut request = valid_request(true);
    request["stream_options"] = json!({ "include_usage": true });
    request
}

fn chat_completions_request(body: Value) -> Request<Body> {
    chat_completions_request_with_headers(body, &[])
}

fn raw_chat_completions_request(body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("OpenAI request should be valid")
}

fn chat_completions_request_with_headers(body: Value, headers: &[(&str, &str)]) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(
            HeaderName::from_bytes(name.as_bytes()).expect("mock header name should be valid"),
            HeaderValue::from_str(value).expect("mock header value should be valid"),
        );
    }

    request
        .body(Body::from(body.to_string()))
        .expect("OpenAI request should be valid")
}

async fn assert_openai_invalid_request(response: axum::response::Response, expected_message: &str) {
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_json(response).await,
        json!({
            "error": {
                "message": expected_message,
                "type": "invalid_request_error",
                "param": null,
                "code": null
            }
        })
    );
}

fn expected_usage(prompt_tokens: u64, completion_tokens: u64, cached_tokens: u64) -> Value {
    json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": prompt_tokens + completion_tokens,
        "prompt_tokens_details": {
            "cached_tokens": cached_tokens,
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

async fn response_json(response: axum::response::Response) -> Value {
    serde_json::from_slice(&response_body(response).await)
        .expect("OpenAI response should contain valid JSON")
}

async fn response_body(response: axum::response::Response) -> Bytes {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("OpenAI response body should be readable")
}

fn data_events(body: Bytes) -> Vec<Value> {
    String::from_utf8(body.to_vec())
        .expect("OpenAI SSE response should contain UTF-8")
        .trim_end()
        .split("\n\n")
        .map(|event| {
            let data = event
                .strip_prefix("data: ")
                .expect("OpenAI SSE event should contain data");
            if data == "[DONE]" {
                return Value::String(data.to_string());
            }

            serde_json::from_str(data).expect("OpenAI SSE data should contain valid JSON")
        })
        .collect()
}

fn streamed_text(events: &[Value]) -> String {
    events
        .iter()
        .filter_map(|event| event["choices"][0]["delta"]["content"].as_str())
        .collect()
}
