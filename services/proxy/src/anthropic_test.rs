use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aws_sdk_dynamodb::config::BehaviorVersion;
use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::http::header::{CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING};
use futures_util::{StreamExt, stream};

use crate::anthropic::{
    AnthropicStreamUsage, AnthropicUsage, ConnectionHeaderNames, anthropic_usage_from_json_slice,
    completed_tokens, should_forward_request_header, should_forward_response_header, test_header,
    test_usage_recording_stream,
};
use crate::background_tasks::BackgroundTasks;
use crate::engineer_auth::AuthenticatedEngineer;
use crate::streams::ActiveStreamTracker;
use crate::token_reconciliation::TokenReconciliationQueue;
use crate::token_reservation::{TokenReservation, TokenReservationManager};
use crate::token_usage::TokenUsageChecker;

#[test]
fn strips_internal_and_hop_by_hop_request_headers() {
    let connection_headers = ConnectionHeaderNames::default();

    assert!(!should_forward_request_header(
        &test_header("x-api-key"),
        &connection_headers
    ));
    assert!(!should_forward_request_header(&HOST, &connection_headers));
    assert!(!should_forward_request_header(
        &CONTENT_LENGTH,
        &connection_headers
    ));
    assert!(!should_forward_request_header(
        &CONNECTION,
        &connection_headers
    ));
    assert!(!should_forward_request_header(
        &TRANSFER_ENCODING,
        &connection_headers
    ));
}

#[test]
fn reconciles_completed_client_errors_as_zero_usage() {
    let body = br#"{"type":"error","error":{"type":"rate_limit_error"}}"#;

    for status in [400, 401, 403, 429] {
        assert_eq!(completed_tokens(status, body), Some(0));
    }
}

#[test]
fn keeps_server_errors_without_usage_indeterminate() {
    assert_eq!(
        completed_tokens(500, br#"{"type":"error","error":{"type":"api_error"}}"#),
        None
    );
}

#[test]
fn forwards_provider_request_headers() {
    let connection_headers = ConnectionHeaderNames::default();

    assert!(should_forward_request_header(
        &test_header("anthropic-version"),
        &connection_headers
    ));
    assert!(should_forward_request_header(
        &test_header("anthropic-beta"),
        &connection_headers
    ));
    assert!(should_forward_request_header(
        &test_header("content-type"),
        &connection_headers
    ));
}

#[test]
fn strips_connection_nominated_request_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(CONNECTION, "x-debug, x-extra-hop".parse().unwrap());
    let connection_headers = ConnectionHeaderNames::from_headers(&headers);

    assert!(!should_forward_request_header(
        &test_header("x-debug"),
        &connection_headers
    ));
    assert!(!should_forward_request_header(
        &test_header("x-extra-hop"),
        &connection_headers
    ));
}

#[test]
fn strips_streaming_response_headers_that_axum_recomputes() {
    let connection_headers = ConnectionHeaderNames::default();

    assert!(!should_forward_response_header(
        &CONTENT_LENGTH,
        &connection_headers
    ));
    assert!(!should_forward_response_header(
        &CONNECTION,
        &connection_headers
    ));
    assert!(!should_forward_response_header(
        &TRANSFER_ENCODING,
        &connection_headers
    ));
}

#[test]
fn forwards_provider_response_headers() {
    let connection_headers = ConnectionHeaderNames::default();

    assert!(should_forward_response_header(
        &test_header("content-type"),
        &connection_headers
    ));
    assert!(should_forward_response_header(
        &test_header("request-id"),
        &connection_headers
    ));
    assert!(should_forward_response_header(
        &test_header("anthropic-ratelimit-requests-limit"),
        &connection_headers
    ));
}

#[test]
fn strips_connection_nominated_response_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(CONNECTION, "x-provider-hop".parse().unwrap());
    let connection_headers = ConnectionHeaderNames::from_headers(&headers);

    assert!(!should_forward_response_header(
        &test_header("x-provider-hop"),
        &connection_headers
    ));
}

#[test]
fn extracts_usage_from_non_streaming_response_body() {
    let usage = anthropic_usage_from_json_slice(
        br#"{"id":"msg_123","usage":{"input_tokens":25,"output_tokens":15}}"#,
    )
    .expect("usage should parse");

    assert_eq!(
        usage,
        AnthropicUsage {
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            input_tokens: Some(25),
            output_tokens: Some(15),
        }
    );
    assert_eq!(usage.total_tokens(), 40);
}

#[test]
fn extracts_cached_usage_from_non_streaming_response_body() {
    let usage = anthropic_usage_from_json_slice(
		br#"{"id":"msg_123","usage":{"input_tokens":25,"cache_creation_input_tokens":100,"cache_read_input_tokens":75,"output_tokens":15}}"#,
	)
	.expect("usage should parse");

    assert_eq!(
        usage,
        AnthropicUsage {
            cache_creation_input_tokens: Some(100),
            cache_read_input_tokens: Some(75),
            input_tokens: Some(25),
            output_tokens: Some(15),
        }
    );
    assert_eq!(usage.total_tokens(), 215);
}

#[test]
fn ignores_non_streaming_response_without_usage() {
    assert!(anthropic_usage_from_json_slice(br#"{"type":"error"}"#).is_none());
}

#[test]
fn extracts_usage_from_streaming_events() {
    let mut usage = AnthropicStreamUsage::default();
    let mut buffer = Vec::new();

    usage.observe_chunk(
        br#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":25,"output_tokens":1}}}

event: message_delta
data: {"type":"message_delta","usage":{"output_tokens":15}}

"#,
        &mut buffer,
    );

    assert_eq!(
        usage.observed_usage(),
        Some(AnthropicUsage {
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            input_tokens: Some(25),
            output_tokens: Some(15),
        })
    );
}

#[test]
fn extracts_cached_usage_from_streaming_events() {
    let mut usage = AnthropicStreamUsage::default();
    let mut buffer = Vec::new();

    usage.observe_chunk(
		br#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":25,"cache_creation_input_tokens":100,"cache_read_input_tokens":75,"output_tokens":1}}}

event: message_delta
data: {"type":"message_delta","usage":{"output_tokens":15}}

"#,
		&mut buffer,
	);

    let usage = usage.observed_usage().expect("usage should parse");

    assert_eq!(
        usage,
        AnthropicUsage {
            cache_creation_input_tokens: Some(100),
            cache_read_input_tokens: Some(75),
            input_tokens: Some(25),
            output_tokens: Some(15),
        }
    );
    assert_eq!(usage.total_tokens(), 215);
}

#[test]
fn extracts_usage_from_split_streaming_events() {
    let mut usage = AnthropicStreamUsage::default();
    let mut buffer = Vec::new();

    usage.observe_chunk(
        br#"event: message_start
data: {"type":"message_start","message":{"usage":{"input_tokens":"#,
        &mut buffer,
    );
    usage.observe_chunk(
        br#"25,"output_tokens":1}}}

event: message_delta
data: {"type":"message_delta","usage":{"output_tokens":15}}

"#,
        &mut buffer,
    );

    assert_eq!(
        usage.observed_usage(),
        Some(AnthropicUsage {
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            input_tokens: Some(25),
            output_tokens: Some(15),
        })
    );
}

#[tokio::test]
async fn drains_provider_stream_after_downstream_disconnects() {
    let consumed_chunks = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&consumed_chunks);
    let provider_stream = stream::iter((0..20).map(move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(Bytes::from_static(b"event: ping\n\n"))
    }));
    let active_streams = Arc::new(ActiveStreamTracker::new(1));
    let stream_guard = active_streams
        .try_start_owned()
        .expect("stream slot should be available");
    let background_tasks = BackgroundTasks::new();
    let reservation = untracked_reservation().await;
    let mut downstream = Box::pin(test_usage_recording_stream(
        provider_stream,
        reservation,
        background_tasks.clone(),
        stream_guard,
    ));

    downstream
        .next()
        .await
        .expect("first downstream chunk should arrive")
        .expect("first downstream chunk should be valid");
    drop(downstream);

    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while consumed_chunks.load(Ordering::SeqCst) < 20 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider drain should finish");
    background_tasks.shutdown().await;

    assert_eq!(consumed_chunks.load(Ordering::SeqCst), 20);
    assert_eq!(active_streams.current(), 0);
}

#[tokio::test]
async fn shutdown_cancels_a_stalled_provider_drain() {
    let provider_stream = stream::pending::<Result<Bytes, reqwest::Error>>();
    let active_streams = Arc::new(ActiveStreamTracker::new(1));
    let stream_guard = active_streams
        .try_start_owned()
        .expect("stream slot should be available");
    let background_tasks = BackgroundTasks::new();
    let reservation = untracked_reservation().await;
    let downstream = test_usage_recording_stream(
        provider_stream,
        reservation,
        background_tasks.clone(),
        stream_guard,
    );

    drop(downstream);
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        background_tasks.shutdown(),
    )
    .await
    .expect("shutdown should cancel the stalled provider drain");

    assert_eq!(active_streams.current(), 0);
}

async fn untracked_reservation() -> TokenReservation {
    let token_usage_checker = Arc::new(TokenUsageChecker::new(
        aws_sdk_dynamodb::Client::from_conf(
            aws_sdk_dynamodb::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .build(),
        ),
        "token-usage",
    ));
    let manager = Arc::new(TokenReservationManager::new(
        aws_sdk_dynamodb::Client::from_conf(
            aws_sdk_dynamodb::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .build(),
        ),
        "token-usage",
        TokenReconciliationQueue::new(
            aws_sdk_sqs::Client::from_conf(
                aws_sdk_sqs::Config::builder()
                    .behavior_version(BehaviorVersion::latest())
                    .build(),
            ),
            "https://sqs.eu-north-1.amazonaws.com/123/token-reconciliation",
        ),
        token_usage_checker,
    ));
    manager
        .reserve(
            AuthenticatedEngineer {
                daily_token_limit: None,
                enabled: true,
                user_id: "engineer-1".to_string(),
                weekly_token_limit: None,
            },
            100,
        )
        .await
        .expect("unlimited engineer reservation should be created")
}
