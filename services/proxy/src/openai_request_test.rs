use std::io::{self, Cursor, Read};

use axum::body::{Body, to_bytes};
use serde_json::Value;

use crate::openai_request::{prepare_openai_request, transform_reader, transform_slice};

#[test]
fn forces_usage_in_streaming_requests_and_preserves_options() {
    let (body, streaming, max_output_tokens) = transform_slice(
        br#"{"model":"gpt-5","stream":true,"stream_options":{"include_obfuscation":false}}"#,
    )
    .expect("request should transform");
    let value: Value = serde_json::from_slice(&body).expect("rewritten request should be JSON");

    assert!(streaming);
    assert_eq!(max_output_tokens, 32_768);
    assert_eq!(value["stream_options"]["include_usage"], true);
    assert_eq!(value["stream_options"]["include_obfuscation"], false);
}

#[test]
fn handles_stream_options_before_stream() {
    let (body, streaming, _) = transform_slice(
        br#"{"stream_options":{"include_usage":false},"model":"gpt-5","stream":true}"#,
    )
    .expect("request should transform");
    let value: Value = serde_json::from_slice(&body).expect("rewritten request should be JSON");

    assert!(streaming);
    assert_eq!(value["stream_options"]["include_usage"], true);
}

#[test]
fn preserves_non_streaming_request_semantics() {
    let original = br#"{ "model": "gpt-5", "stream": false, "messages": [{ "role": "user", "content": "hello" }] }"#;
    let (body, streaming, max_output_tokens) =
        transform_slice(original).expect("request should transform");

    assert!(!streaming);
    let value = serde_json::from_slice::<Value>(&body).unwrap();
    assert_eq!(value["model"], "gpt-5");
    assert_eq!(value["stream"], false);
    assert_eq!(value["messages"][0]["content"], "hello");
    assert_eq!(value["max_completion_tokens"], 32_768);
    assert_eq!(max_output_tokens, 32_768);
}

#[test]
fn rejects_non_object_stream_options_for_streaming_requests() {
    let error = transform_slice(br#"{"model":"gpt-5","stream":true,"stream_options":"invalid"}"#)
        .expect_err("invalid stream options should fail");

    assert_eq!(
        error.to_string(),
        "OpenAI stream_options must be a JSON object"
    );
}

#[test]
fn rejects_duplicate_control_fields() {
    let error = transform_slice(br#"{"stream":false,"stream":true}"#)
        .expect_err("duplicate stream fields should fail closed");

    assert_eq!(
        error.to_string(),
        "OpenAI request contains duplicate stream fields"
    );
}

#[test]
fn transforms_input_split_across_small_reads() {
    let input = ChunkedReader::new(
        br#"{"model":"gpt-5","messages":[{"content":"hello"}],"stream":true}"#,
        3,
    );
    let mut output = Vec::new();

    let streaming = transform_reader(input, &mut output).expect("request should transform");
    let value: Value = serde_json::from_slice(&output).expect("rewritten request should be JSON");

    assert!(streaming);
    assert_eq!(value["stream_options"]["include_usage"], true);
    assert_eq!(value["messages"][0]["content"], "hello");
}

#[tokio::test]
async fn streams_between_axum_and_reqwest_bodies() {
    let transformed = prepare_openai_request(
        Body::from(r#"{"model":"gpt-5","messages":[{"content":"hello"}],"stream":true}"#),
        32_768,
    )
    .await
    .expect("request should prepare");

    let (body, streaming, token_budget) = transformed.into_parts();
    let output = to_bytes(Body::new(body), 1024)
        .await
        .expect("transformed body should stream");

    let value: Value = serde_json::from_slice(&output).expect("output should be valid JSON");

    assert!(streaming);
    assert_eq!(token_budget, output.len() as u64 + 32_768);
    assert_eq!(value["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn adds_a_conservative_reservation_for_image_inputs() {
    let transformed = prepare_openai_request(
        Body::from(
            r#"{"model":"gpt-5","messages":[{"content":[{"type":"image_url","image_url":{"url":"https://example.com/image.png"}}]}],"max_completion_tokens":5000}"#,
        ),
        32_768,
    )
    .await
    .expect("image request should prepare");

    let (body, _, token_budget) = transformed.into_parts();
    let output = to_bytes(Body::new(body), 4096)
        .await
        .expect("prepared body should be readable");

    assert_eq!(token_budget, output.len() as u64 + 5_000 + 32_768);
}

#[tokio::test]
async fn detects_escaped_image_input_keys() {
    let transformed = prepare_openai_request(
        Body::from(
            r#"{"model":"gpt-5","messages":[{"content":[{"image\u005furl":{"url":"https://example.com/image.png"}}]}],"max_completion_tokens":5000}"#,
        ),
        32_768,
    )
    .await
    .expect("escaped image request should prepare");

    let (body, _, token_budget) = transformed.into_parts();
    let output = to_bytes(Body::new(body), 4096)
        .await
        .expect("prepared body should be readable");

    assert_eq!(token_budget, output.len() as u64 + 5_000 + 32_768);
}

#[test]
fn preserves_explicit_completion_limit_for_reservation() {
    let (body, _, max_output_tokens) =
        transform_slice(br#"{"model":"gpt-5","messages":[],"max_completion_tokens":5000}"#)
            .expect("request should transform");
    let value: Value = serde_json::from_slice(&body).expect("request should remain JSON");

    assert_eq!(value["max_completion_tokens"], 5000);
    assert_eq!(max_output_tokens, 5000);
}

#[test]
fn streams_requests_larger_than_the_old_twenty_mebibyte_limit() {
    let content_bytes = 21 * 1024 * 1024;
    let input = Cursor::new(br#"{"model":"gpt-5","messages":[{"content":""#)
        .chain(io::repeat(b'a').take(content_bytes))
        .chain(Cursor::new(br#""}],"stream":true}"#));

    let streaming = transform_reader(input, io::sink()).expect("large request should stream");

    assert!(streaming);
}

#[test]
fn accepts_json_keys_at_the_size_limit() {
    let key = "a".repeat(8 * 1024);
    let request = format!(r#"{{"{key}":true}}"#);

    transform_slice(request.as_bytes()).expect("key at the size limit should be accepted");
}

#[test]
fn rejects_json_keys_above_the_size_limit() {
    let key = "a".repeat(8 * 1024 + 1);
    let request = format!(r#"{{"{key}":true}}"#);

    let error = transform_slice(request.as_bytes()).expect_err("large key should be rejected");

    assert!(error.to_string().contains("key exceeds its size limit"));
}

struct ChunkedReader<'a> {
    bytes: &'a [u8],
    chunk_size: usize,
    offset: usize,
}

impl<'a> ChunkedReader<'a> {
    fn new(bytes: &'a [u8], chunk_size: usize) -> Self {
        Self {
            bytes,
            chunk_size,
            offset: 0,
        }
    }
}

impl Read for ChunkedReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.offset == self.bytes.len() {
            return Ok(0);
        }

        let count = output
            .len()
            .min(self.chunk_size)
            .min(self.bytes.len() - self.offset);
        output[..count].copy_from_slice(&self.bytes[self.offset..self.offset + count]);
        self.offset += count;
        Ok(count)
    }
}
