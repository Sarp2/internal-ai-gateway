use std::io;
use std::time::Duration;

use axum::body::{Body, Bytes};
use futures_util::stream;

use struson::reader::ReaderError;

use crate::anthropic_request::{AnthropicRequestError, inspect_slice, prepare_with_upload_timeout};

#[tokio::test]
async fn rejects_request_bodies_that_exceed_the_upload_deadline() {
    let body = Body::from_stream(stream::pending::<Result<Bytes, io::Error>>());

    let error = prepare_with_upload_timeout(body, Duration::from_millis(10))
        .await
        .expect_err("stalled upload should time out");

    assert!(error.is_upload_timeout());
}

#[test]
fn extracts_reservation_controls_without_changing_the_request() {
    let (streaming, max_tokens, image_inputs) = inspect_slice(
        br#"{"model":"claude-sonnet-4-5","max_tokens":4096,"stream":true,"messages":[{"role":"user","content":"hello"}]}"#,
    )
    .expect("request metadata should parse");

    assert!(streaming);
    assert_eq!(max_tokens, 4096);
    assert_eq!(image_inputs, 0);
}

#[test]
fn counts_anthropic_image_content_blocks() {
    let (_, _, image_inputs) = inspect_slice(
        br#"{"model":"claude-sonnet-4-5","max_tokens":1024,"messages":[{"role":"user","content":[{"type":"image","source":{"type":"url","url":"https://example.com/one.png"}},{"type":"text","text":"compare"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"abc"}}]}]}"#,
    )
    .expect("image request should parse");

    assert_eq!(image_inputs, 2);
}

#[test]
fn rejects_missing_or_invalid_max_tokens() {
    assert!(inspect_slice(br#"{"model":"claude-sonnet-4-5","messages":[]}"#).is_err());
    assert!(
        inspect_slice(br#"{"model":"claude-sonnet-4-5","max_tokens":0,"messages":[]}"#).is_err()
    );
}

#[test]
fn rejects_duplicate_max_tokens() {
    assert!(
        inspect_slice(
            br#"{"model":"claude-sonnet-4-5","max_tokens":100,"max_tokens":200,"messages":[]}"#
        )
        .is_err()
    );
}

#[test]
fn rejects_json_above_the_explicit_nesting_limit() {
    let nested_value = format!("{}null{}", "[".repeat(129), "]".repeat(129));
    let request = format!(r#"{{"max_tokens":1,"messages":{nested_value}}}"#);

    let error = inspect_slice(request.as_bytes()).expect_err("deep JSON should be rejected");

    assert!(matches!(
        error,
        AnthropicRequestError::InvalidJson(ReaderError::MaxNestingDepthExceeded {
            max_nesting_depth: 128,
            ..
        })
    ));
}
