use std::io;
use std::time::Duration;

use aws_sdk_dynamodb::types::AttributeValue;
use integration_tests::anthropic_messages::AnthropicMessagesEnvironment;
use integration_tests::fixtures::engineer::{
    EngineerFixtureOptions, FixtureError, with_engineer_fixture,
};
use integration_tests::token_windows::{
    daily_usage_window, stable_usage_window_time, weekly_usage_window,
};
use reqwest::StatusCode;
use tokio::time::{sleep, timeout};

const EXPECTED_TOTAL_TOKENS: u64 = 18;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLL_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_LIMIT: u64 = 10_000;

#[tokio::test]
async fn reserves_budget_then_reconciles_exact_usage() -> Result<(), FixtureError> {
    let environment = AnthropicMessagesEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        EngineerFixtureOptions {
            daily_token_limit: Some(TOKEN_LIMIT),
            weekly_token_limit: Some(TOKEN_LIMIT),
            ..EngineerFixtureOptions::default()
        },
        |engineer| async move {
            let request_time = stable_usage_window_time().await?;
            let response = test_environment
                .send_messages_with_options(
                    engineer.api_key(),
                    true,
                    &[("x-mock-chunk-delay-ms", "500")],
                )
                .await?;
            assert_eq!(response.status(), StatusCode::OK);

            let daily_window = daily_usage_window(request_time);
            let weekly_window = weekly_usage_window(request_time);
            let reserved_daily = wait_for_usage(
                test_environment,
                engineer.user_id(),
                &daily_window,
                |usage| usage.reserved_tokens > 0 && usage.consumed_tokens == 0,
            )
            .await?;
            let reserved_weekly = wait_for_usage(
                test_environment,
                engineer.user_id(),
                &weekly_window,
                |usage| usage.reserved_tokens > 0 && usage.consumed_tokens == 0,
            )
            .await?;

            assert_eq!(reserved_daily.token_count, reserved_daily.reserved_tokens);
            assert_eq!(reserved_weekly, reserved_daily);
            assert!(reserved_daily.reserved_tokens > EXPECTED_TOTAL_TOKENS);

            response.bytes().await?;

            let completed_daily = wait_for_usage(
                test_environment,
                engineer.user_id(),
                &daily_window,
                |usage| {
                    usage.reserved_tokens == 0 && usage.consumed_tokens == EXPECTED_TOTAL_TOKENS
                },
            )
            .await?;
            let completed_weekly = wait_for_usage(
                test_environment,
                engineer.user_id(),
                &weekly_window,
                |usage| {
                    usage.reserved_tokens == 0 && usage.consumed_tokens == EXPECTED_TOTAL_TOKENS
                },
            )
            .await?;

            assert_eq!(completed_daily.token_count, EXPECTED_TOTAL_TOKENS);
            assert_eq!(completed_weekly, completed_daily);
            Ok(())
        },
    )
    .await
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UsageCounters {
    consumed_tokens: u64,
    reserved_tokens: u64,
    token_count: u64,
}

async fn wait_for_usage(
    environment: &AnthropicMessagesEnvironment,
    user_id: &str,
    usage_window: &str,
    predicate: impl Fn(UsageCounters) -> bool,
) -> Result<UsageCounters, FixtureError> {
    timeout(POLL_TIMEOUT, async {
        loop {
            if let Some(usage) = read_usage(environment, user_id, usage_window).await?
                && predicate(usage)
            {
                return Ok(usage);
            }
            sleep(POLL_INTERVAL).await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("usage window {usage_window} did not reach the expected state"),
        )
    })?
}

async fn read_usage(
    environment: &AnthropicMessagesEnvironment,
    user_id: &str,
    usage_window: &str,
) -> Result<Option<UsageCounters>, FixtureError> {
    let response = environment
        .dynamodb_client
        .get_item()
        .table_name(&environment.config.token_usage_table_name)
        .key("user_id", AttributeValue::S(user_id.to_string()))
        .key("usage_window", AttributeValue::S(usage_window.to_string()))
        .consistent_read(true)
        .send()
        .await?;

    response
        .item()
        .map(|item| {
            Ok(UsageCounters {
                consumed_tokens: number_or_zero(item.get("consumed_tokens"))?,
                reserved_tokens: number_or_zero(item.get("reserved_tokens"))?,
                token_count: number_or_zero(item.get("token_count"))?,
            })
        })
        .transpose()
}

fn number_or_zero(attribute: Option<&AttributeValue>) -> Result<u64, FixtureError> {
    match attribute {
        Some(AttributeValue::N(value)) => Ok(value.parse()?),
        None => Ok(0),
        Some(_) => Err(io::Error::other("usage counter is not a DynamoDB number").into()),
    }
}
