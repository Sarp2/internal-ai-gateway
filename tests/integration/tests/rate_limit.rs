use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_sdk_dynamodb::types::AttributeValue;
use integration_tests::anthropic_messages::AnthropicMessagesEnvironment;
use integration_tests::fixtures::engineer::{
    EngineerFixtureOptions, FixtureError, with_engineer_fixture,
};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};
use tokio::time::sleep;

#[tokio::test]
async fn allows_final_request_and_rejects_request_above_limit() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        EngineerFixtureOptions::default(),
        |engineer| async move {
            seed_current_window(
                test_environment,
                engineer.user_id(),
                test_environment.config.rate_limit_requests_per_window - 1,
            )
            .await?;

            let allowed = test_environment.send_messages(engineer.api_key()).await?;
            assert_eq!(allowed.status(), StatusCode::OK);

            let rejected = test_environment.send_messages(engineer.api_key()).await?;
            assert_rate_limit_error(rejected).await
        },
    )
    .await
}

#[tokio::test]
async fn keeps_rate_limits_independent_between_engineers() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        EngineerFixtureOptions::default(),
        |limited_engineer| async move {
            with_engineer_fixture(
                &test_environment.config,
                &test_environment.sdk_config,
                EngineerFixtureOptions::default(),
                |other_engineer| async move {
                    seed_current_window(
                        test_environment,
                        limited_engineer.user_id(),
                        test_environment.config.rate_limit_requests_per_window,
                    )
                    .await?;

                    let rejected = test_environment
                        .send_messages(limited_engineer.api_key())
                        .await?;
                    assert_rate_limit_error(rejected).await?;

                    let allowed = test_environment
                        .send_messages(other_engineer.api_key())
                        .await?;
                    assert_eq!(allowed.status(), StatusCode::OK);
                    Ok(())
                },
            )
            .await
        },
    )
    .await
}

#[tokio::test]
async fn allows_requests_in_a_new_rate_limit_window() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        EngineerFixtureOptions::default(),
        |engineer| async move {
            let window_seconds = test_environment.config.rate_limit_window_seconds;
            let current_window = stable_current_window(window_seconds).await?;
            seed_window(
                test_environment,
                engineer.user_id(),
                current_window - window_seconds,
                test_environment.config.rate_limit_requests_per_window,
            )
            .await?;

            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_eq!(response.status(), StatusCode::OK);
            Ok(())
        },
    )
    .await
}

async fn seed_current_window(
    environment: &AnthropicMessagesEnvironment,
    user_id: &str,
    request_count: u64,
) -> Result<(), FixtureError> {
    seed_window(
        environment,
        user_id,
        stable_current_window(environment.config.rate_limit_window_seconds).await?,
        request_count,
    )
    .await
}

async fn seed_window(
    environment: &AnthropicMessagesEnvironment,
    user_id: &str,
    window_start: u64,
    request_count: u64,
) -> Result<(), FixtureError> {
    environment
        .dynamodb_client
        .put_item()
        .table_name(&environment.config.rate_limit_table_name)
        .item("user_id", AttributeValue::S(user_id.to_string()))
        .item("request_ts", AttributeValue::N(window_start.to_string()))
        .item(
            "request_count",
            AttributeValue::N(request_count.to_string()),
        )
        .item(
            "ttl",
            AttributeValue::N(
                (window_start + environment.config.rate_limit_window_seconds * 2).to_string(),
            ),
        )
        .send()
        .await?;
    Ok(())
}

async fn stable_current_window(window_seconds: u64) -> Result<u64, FixtureError> {
    let mut now = current_epoch_seconds()?;
    let seconds_remaining = window_seconds - (now % window_seconds);

    if seconds_remaining <= 5 {
        sleep(Duration::from_secs(seconds_remaining + 1)).await;
        now = current_epoch_seconds()?;
    }

    Ok(now - (now % window_seconds))
}

fn current_epoch_seconds() -> Result<u64, FixtureError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

async fn assert_rate_limit_error(response: Response) -> Result<(), FixtureError> {
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = response.json::<Value>().await?;
    assert_eq!(body, json!({ "message": "rate limit exceeded" }));
    Ok(())
}
