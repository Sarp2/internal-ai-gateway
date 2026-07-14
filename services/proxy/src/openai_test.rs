use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use aws_sdk_dynamodb::config::BehaviorVersion;
use axum::body::Bytes;
use axum::http::StatusCode;
use axum::http::header::{ACCEPT_ENCODING, AUTHORIZATION, CONNECTION};
use axum::http::{HeaderMap, HeaderName};
use futures_util::{StreamExt, stream};

use crate::background_tasks::BackgroundTasks;
use crate::engineer_auth::AuthenticatedEngineer;
use crate::openai::{
    OpenAiStreamUsage, OpenAiUsage, forwards_request_header, openai_usage_from_json_slice,
    request_headers_recomputed_by_client, streams_provider_response, test_usage_recording_stream,
};
use crate::streams::ActiveStreamTracker;
use crate::token_reconciliation::TokenReconciliationQueue;
use crate::token_reservation::TokenReservationManager;
use crate::token_usage::TokenUsageChecker;

#[test]
fn strips_gateway_and_provider_credentials_from_forwarded_headers() {
    let headers = HeaderMap::new();

    assert!(!forwards_request_header(
        &HeaderName::from_static("x-api-key"),
        &headers
    ));
    assert!(!forwards_request_header(&AUTHORIZATION, &headers));
    assert!(!forwards_request_header(&ACCEPT_ENCODING, &headers));

    for header in request_headers_recomputed_by_client() {
        assert!(!forwards_request_header(&header, &headers));
    }
}

#[test]
fn strips_provider_billing_scope_headers() {
    let headers = HeaderMap::new();

    assert!(!forwards_request_header(
        &HeaderName::from_static("openai-organization"),
        &headers
    ));
    assert!(!forwards_request_header(
        &HeaderName::from_static("openai-project"),
        &headers
    ));
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
fn streams_successful_responses_when_requested() {
    assert!(streams_provider_response(true, StatusCode::OK));
    assert!(!streams_provider_response(false, StatusCode::OK));
    assert!(!streams_provider_response(true, StatusCode::BAD_REQUEST));
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

#[tokio::test]
async fn drains_provider_stream_after_downstream_disconnects() {
    let consumed_chunks = Arc::new(AtomicUsize::new(0));
    let provider_counter = Arc::clone(&consumed_chunks);
    let provider_stream = stream::iter((0..20).map(move |index| {
        provider_counter.fetch_add(1, Ordering::SeqCst);
        Ok::<_, reqwest::Error>(Bytes::from(format!("data: chunk-{index}\n\n")))
    }));
    let active_streams = Arc::new(ActiveStreamTracker::new(1));
    let stream_guard = active_streams
        .try_start_owned()
        .expect("stream slot should be available");
    let background_tasks = BackgroundTasks::new();
    let token_usage_checker = Arc::new(TokenUsageChecker::new(
        aws_sdk_dynamodb::Client::from_conf(
            aws_sdk_dynamodb::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .build(),
        ),
        "token-usage",
    ));
    let engineer = AuthenticatedEngineer {
        daily_token_limit: None,
        enabled: true,
        user_id: "engineer-1".to_string(),
        weekly_token_limit: None,
    };
    let token_reservation_manager = Arc::new(TokenReservationManager::new(
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
        Arc::clone(&token_usage_checker),
    ));
    let reservation = token_reservation_manager
        .reserve(engineer, 100)
        .await
        .expect("unlimited engineer reservation should be created");
    let mut downstream = Box::pin(test_usage_recording_stream(
        provider_stream,
        reservation,
        stream_guard,
        background_tasks.clone(),
    ));

    downstream
        .next()
        .await
        .expect("first downstream chunk should arrive")
        .expect("first downstream chunk should be valid");
    drop(downstream);

    background_tasks.shutdown().await;

    assert_eq!(consumed_chunks.load(Ordering::SeqCst), 20);
    assert_eq!(active_streams.current(), 0);
}
