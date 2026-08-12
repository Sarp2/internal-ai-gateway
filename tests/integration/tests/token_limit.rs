use aws_sdk_dynamodb::types::AttributeValue;
use integration_tests::anthropic_messages::AnthropicMessagesEnvironment;
use integration_tests::fixtures::engineer::{
    EngineerFixtureOptions, FixtureError, with_engineer_fixture,
};
use integration_tests::token_windows::{
    daily_usage_window, stable_usage_window_time, weekly_usage_window,
};
use reqwest::{Response, StatusCode};
use serde_json::{Value, json};

const EXPECTED_TOTAL_TOKENS: u64 = 18;
const TOKEN_LIMIT: u64 = 10_000;

#[tokio::test]
async fn allows_engineer_without_token_limits() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        EngineerFixtureOptions::default(),
        |engineer| async move {
            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_eq!(response.status(), StatusCode::OK);
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn allows_request_with_available_daily_and_weekly_quota() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        token_limits(Some(TOKEN_LIMIT), Some(TOKEN_LIMIT)),
        |engineer| async move {
            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_eq!(response.status(), StatusCode::OK);
            Ok(())
        },
    )
    .await
}

#[tokio::test]
async fn rejects_engineer_with_exhausted_daily_quota() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        token_limits(Some(TOKEN_LIMIT), None),
        |engineer| async move {
            let request_time = stable_usage_window_time().await?;
            seed_usage(
                test_environment,
                engineer.user_id(),
                daily_usage_window(request_time),
                TOKEN_LIMIT,
            )
            .await?;

            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_token_limit_error(response).await
        },
    )
    .await
}

#[tokio::test]
async fn rejects_engineer_with_exhausted_weekly_quota() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        token_limits(None, Some(TOKEN_LIMIT)),
        |engineer| async move {
            let request_time = stable_usage_window_time().await?;
            seed_usage(
                test_environment,
                engineer.user_id(),
                weekly_usage_window(request_time),
                TOKEN_LIMIT,
            )
            .await?;

            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_token_limit_error(response).await
        },
    )
    .await
}

#[tokio::test]
async fn keeps_token_limits_independent_between_engineers() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        token_limits(Some(TOKEN_LIMIT), None),
        |limited_engineer| async move {
            with_engineer_fixture(
                &test_environment.config,
                &test_environment.sdk_config,
                EngineerFixtureOptions::default(),
                |other_engineer| async move {
                    let request_time = stable_usage_window_time().await?;
                    seed_usage(
                        test_environment,
                        limited_engineer.user_id(),
                        daily_usage_window(request_time),
                        TOKEN_LIMIT,
                    )
                    .await?;

                    let rejected = test_environment
                        .send_messages(limited_engineer.api_key())
                        .await?;
                    assert_token_limit_error(rejected).await?;

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
async fn records_exact_usage_in_daily_and_weekly_windows() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        token_limits(Some(TOKEN_LIMIT), Some(TOKEN_LIMIT)),
        |engineer| async move {
            let request_time = stable_usage_window_time().await?;
            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_eq!(response.status(), StatusCode::OK);

            assert_usage(
                test_environment,
                engineer.user_id(),
                &daily_usage_window(request_time),
                EXPECTED_TOTAL_TOKENS,
            )
            .await?;
            assert_usage(
                test_environment,
                engineer.user_id(),
                &weekly_usage_window(request_time),
                EXPECTED_TOTAL_TOKENS,
            )
            .await
        },
    )
    .await
}

async fn seed_usage(
    environment: &AnthropicMessagesEnvironment,
    user_id: &str,
    usage_window: String,
    token_count: u64,
) -> Result<(), FixtureError> {
    environment
        .dynamodb_client
        .put_item()
        .table_name(&environment.config.token_usage_table_name)
        .item("user_id", AttributeValue::S(user_id.to_string()))
        .item("usage_window", AttributeValue::S(usage_window))
        .item("token_count", AttributeValue::N(token_count.to_string()))
        .send()
        .await?;
    Ok(())
}

async fn assert_usage(
    environment: &AnthropicMessagesEnvironment,
    user_id: &str,
    usage_window: &str,
    expected_tokens: u64,
) -> Result<(), FixtureError> {
    let response = environment
        .dynamodb_client
        .get_item()
        .table_name(&environment.config.token_usage_table_name)
        .key("user_id", AttributeValue::S(user_id.to_string()))
        .key("usage_window", AttributeValue::S(usage_window.to_string()))
        .consistent_read(true)
        .send()
        .await?;
    let item = response
        .item()
        .ok_or_else(|| std::io::Error::other(format!("missing usage row {usage_window}")))?;

    assert_number(item.get("token_count"), expected_tokens);
    assert_number(item.get("consumed_tokens"), expected_tokens);
    assert_number(item.get("reserved_tokens"), 0);
    Ok(())
}

fn token_limits(
    daily_token_limit: Option<u64>,
    weekly_token_limit: Option<u64>,
) -> EngineerFixtureOptions {
    EngineerFixtureOptions {
        daily_token_limit,
        weekly_token_limit,
        ..EngineerFixtureOptions::default()
    }
}

fn assert_number(attribute: Option<&AttributeValue>, expected: u64) {
    assert_eq!(attribute, Some(&AttributeValue::N(expected.to_string())));
}

async fn assert_token_limit_error(response: Response) -> Result<(), FixtureError> {
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    let body = response.json::<Value>().await?;
    assert_eq!(
        body,
        json!({ "message": "Anthropic token reservation failed: token limit exceeded" })
    );
    Ok(())
}
