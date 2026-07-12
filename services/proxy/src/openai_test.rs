use axum::http::header::{AUTHORIZATION, CONNECTION};
use axum::http::{HeaderMap, HeaderName};
use serde_json::Value;

use crate::openai::{
    OpenAiStreamUsage, OpenAiUsage, forwards_request_header, openai_usage_from_json_slice,
    prepare_request_body, request_headers_recomputed_by_client,
};

#[test]
fn forces_usage_in_streaming_requests_and_preserves_options() {
    let (body, streaming) = prepare_request_body(
        br#"{"model":"gpt-5","stream":true,"stream_options":{"include_obfuscation":false}}"#,
    )
    .expect("request should parse");
    let value: Value = serde_json::from_slice(&body).expect("rewritten request should be JSON");

    assert!(streaming);
    assert_eq!(value["stream_options"]["include_usage"], true);
    assert_eq!(value["stream_options"]["include_obfuscation"], false);
}

#[test]
fn leaves_non_streaming_request_semantics_unchanged() {
    let original = br#"{ "model": "gpt-5", "messages": [{ "role": "user", "content": "hello" }] }"#;
    let (body, streaming) = prepare_request_body(original).expect("request should parse");

    assert!(!streaming);
    assert_eq!(body, original);
}

#[test]
fn rejects_non_object_stream_options_for_streaming_requests() {
    let error =
        prepare_request_body(br#"{"model":"gpt-5","stream":true,"stream_options":"invalid"}"#)
            .expect_err("invalid stream options should fail");

    assert_eq!(
        error.to_string(),
        "OpenAI stream_options must be a JSON object"
    );
}

#[test]
fn strips_gateway_and_provider_credentials_from_forwarded_headers() {
    let headers = HeaderMap::new();

    assert!(!forwards_request_header(
        &HeaderName::from_static("x-api-key"),
        &headers
    ));
    assert!(!forwards_request_header(&AUTHORIZATION, &headers));

    for header in request_headers_recomputed_by_client() {
        assert!(!forwards_request_header(&header, &headers));
    }
}

#[test]
fn strips_connection_nominated_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(CONNECTION, "x-private-hop".parse().unwrap());

    assert!(!forwards_request_header(
        &HeaderName::from_static("x-private-hop"),
        &headers
    ));
}

#[test]
fn extracts_non_streaming_usage() {
    let usage = openai_usage_from_json_slice(
        br#"{"usage":{"prompt_tokens":25,"completion_tokens":15,"total_tokens":40}}"#,
    )
    .expect("usage should parse");

    assert_eq!(
        usage,
        OpenAiUsage {
            prompt_tokens: Some(25),
            completion_tokens: Some(15),
            total_tokens: 40,
        }
    );
}

#[test]
fn falls_back_to_prompt_plus_completion_tokens() {
    let usage =
        openai_usage_from_json_slice(br#"{"usage":{"prompt_tokens":25,"completion_tokens":15}}"#)
            .expect("usage should parse");

    assert_eq!(usage.total_tokens, 40);
}

#[test]
fn extracts_final_usage_from_streaming_chunks() {
    let mut stream_usage = OpenAiStreamUsage::default();

    stream_usage.observe_chunk(
        br#"data: {"id":"chatcmpl-1","choices":[{"delta":{"content":"hello"}}],"usage":null}

data: {"id":"chatcmpl-1","choices":[],"usage":{"prompt_tokens":25,"completion_tokens":15,"total_tokens":40}}

data: [DONE]

"#,
    );

    assert_eq!(
        stream_usage.observed_usage(),
        Some(OpenAiUsage {
            prompt_tokens: Some(25),
            completion_tokens: Some(15),
            total_tokens: 40,
        })
    );
}

#[test]
fn extracts_usage_when_sse_event_is_split_across_chunks() {
    let mut stream_usage = OpenAiStreamUsage::default();

    stream_usage.observe_chunk(
        br#"data: {"id":"chatcmpl-1","choices":[],"usage":{"prompt_tokens":25,"completion_"#,
    );
    assert!(stream_usage.observed_usage().is_none());

    stream_usage.observe_chunk(
        br#"tokens":15,"total_tokens":40}}

data: [DONE]

"#,
    );

    assert_eq!(stream_usage.observed_usage().unwrap().total_tokens, 40);
}

#[test]
fn has_no_usage_when_stream_ends_before_final_usage_chunk() {
    let mut stream_usage = OpenAiStreamUsage::default();

    stream_usage.observe_chunk(
        br#"data: {"id":"chatcmpl-1","choices":[{"delta":{"content":"hello"}}],"usage":null}

"#,
    );

    assert!(stream_usage.observed_usage().is_none());
}
