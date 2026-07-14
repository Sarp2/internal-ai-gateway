use std::sync::Arc;

use aws_sdk_dynamodb::config::BehaviorVersion;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::anthropic;
use crate::api_key::ApiKeyHasher;
use crate::app::{AppState, app};
use crate::auth::RequestAuthenticator;
use crate::background_tasks::BackgroundTasks;
use crate::engineer_auth::EngineerAuth;
use crate::openai;
use crate::rate_limit::RateLimiter;
use crate::streams::ActiveStreamTracker;
use crate::token_accounting::TokenAccounting;

#[tokio::test]
async fn returns_healthy_status_from_health_route() {
    let response = test_app()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/health")
                .body(Body::empty())
                .expect("health request should build"),
        )
        .await
        .expect("health request should complete");

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("health body should be readable");

    assert_eq!(&body[..], br#"{"status":"ok"}"#);
}

#[tokio::test]
async fn returns_not_found_for_unknown_routes() {
    let response = test_app()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/unknown")
                .body(Body::empty())
                .expect("unknown request should build"),
        )
        .await
        .expect("unknown request should complete");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("not found body should be readable");

    assert_eq!(&body[..], br#"{"message":"Route not found."}"#);
}

fn test_app() -> axum::Router {
    let api_key_hasher = Arc::new(ApiKeyHasher::new("test-secret"));
    let engineer_auth = Arc::new(EngineerAuth::new(
        aws_sdk_dynamodb::Client::from_conf(
            aws_sdk_dynamodb::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .build(),
        ),
        "engineers",
        "ApiKeyIndex",
    ));
    let rate_limiter = Arc::new(RateLimiter::new(
        aws_sdk_dynamodb::Client::from_conf(
            aws_sdk_dynamodb::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .build(),
        ),
        "rate-limits",
        120,
        std::time::Duration::from_secs(60),
    ));
    let token_accounting = TokenAccounting::new(
        aws_sdk_dynamodb::Client::from_conf(
            aws_sdk_dynamodb::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .build(),
        ),
        aws_sdk_sqs::Client::from_conf(
            aws_sdk_sqs::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .build(),
        ),
        "https://sqs.eu-north-1.amazonaws.com/123/token-reconciliation",
        "token-usage",
    );

    app(AppState::new(
        Arc::new(anthropic::test_proxy("test-anthropic-api-key")),
        Arc::new(RequestAuthenticator::new(api_key_hasher, engineer_auth)),
        BackgroundTasks::new(),
        Arc::new(openai::test_proxy("test-openai-api-key")),
        rate_limiter,
        Arc::new(ActiveStreamTracker::new(200)),
        token_accounting,
    ))
}
