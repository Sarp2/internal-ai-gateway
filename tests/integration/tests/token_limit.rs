use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::AttributeValue;
use integration_tests::IntegrationConfig;
use integration_tests::fixtures::engineer::{
    EngineerFixtureOptions, FixtureError, with_engineer_fixture,
};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use url::Url;

const DAILY_WINDOW_SECONDS: u64 = 86_400;
const EXPECTED_TOTAL_TOKENS: u64 = 18;
const MONDAY_WEEK_OFFSET_SECONDS: u64 = DAILY_WINDOW_SECONDS * 3;
const TOKEN_LIMIT: u64 = 10_000;
const WEEKLY_WINDOW_SECONDS: u64 = DAILY_WINDOW_SECONDS * 7;

#[tokio::test]
async fn allows_engineer_without_token_limits() -> Result<(), FixtureError> {
    let environment = TokenLimitEnvironment::load().await?;
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
    let environment = TokenLimitEnvironment::load().await?;
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
    let environment = TokenLimitEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        token_limits(Some(TOKEN_LIMIT), None),
        |engineer| async move {
            test_environment
                .seed_usage(engineer.user_id(), daily_usage_window(now()?), TOKEN_LIMIT)
                .await?;

            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_token_limit_error(response).await
        },
    )
    .await
}

#[tokio::test]
async fn rejects_engineer_with_exhausted_weekly_quota() -> Result<(), FixtureError> {
    let environment = TokenLimitEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        token_limits(None, Some(TOKEN_LIMIT)),
        |engineer| async move {
            test_environment
                .seed_usage(engineer.user_id(), weekly_usage_window(now()?), TOKEN_LIMIT)
                .await?;

            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_token_limit_error(response).await
        },
    )
    .await
}

#[tokio::test]
async fn keeps_token_limits_independent_between_engineers() -> Result<(), FixtureError> {
    let environment = TokenLimitEnvironment::load().await?;
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
                    test_environment
                        .seed_usage(
                            limited_engineer.user_id(),
                            daily_usage_window(now()?),
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
    let environment = TokenLimitEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        token_limits(Some(TOKEN_LIMIT), Some(TOKEN_LIMIT)),
        |engineer| async move {
            let request_time = now()?;
            let response = test_environment.send_messages(engineer.api_key()).await?;
            assert_eq!(response.status(), StatusCode::OK);

            test_environment
                .assert_usage(
                    engineer.user_id(),
                    &daily_usage_window(request_time),
                    EXPECTED_TOTAL_TOKENS,
                )
                .await?;
            test_environment
                .assert_usage(
                    engineer.user_id(),
                    &weekly_usage_window(request_time),
                    EXPECTED_TOTAL_TOKENS,
                )
                .await
        },
    )
    .await
}

struct TokenLimitEnvironment {
    client: Client,
    config: IntegrationConfig,
    dynamodb_client: DynamoDbClient,
    messages_url: Url,
    sdk_config: SdkConfig,
}

impl TokenLimitEnvironment {
    async fn load() -> Result<Self, FixtureError> {
        let config = IntegrationConfig::load().map_err(std::io::Error::other)?;
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.aws_region.clone()))
            .load()
            .await;
        let dynamodb_client = DynamoDbClient::new(&sdk_config);
        let mut messages_url = Url::parse(&config.proxy_health_url)?;
        messages_url.set_path("/v1/anthropic/messages");
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;

        Ok(Self {
            client,
            config,
            dynamodb_client,
            messages_url,
            sdk_config,
        })
    }

    async fn seed_usage(
        &self,
        user_id: &str,
        usage_window: String,
        token_count: u64,
    ) -> Result<(), FixtureError> {
        self.dynamodb_client
            .put_item()
            .table_name(&self.config.token_usage_table_name)
            .item("user_id", AttributeValue::S(user_id.to_string()))
            .item("usage_window", AttributeValue::S(usage_window))
            .item("token_count", AttributeValue::N(token_count.to_string()))
            .send()
            .await?;
        Ok(())
    }

    async fn assert_usage(
        &self,
        user_id: &str,
        usage_window: &str,
        expected_tokens: u64,
    ) -> Result<(), FixtureError> {
        let response = self
            .dynamodb_client
            .get_item()
            .table_name(&self.config.token_usage_table_name)
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

    async fn send_messages(&self, api_key: &str) -> Result<Response, FixtureError> {
        Ok(self
            .client
            .post(self.messages_url.clone())
            .header("x-api-key", api_key)
            .json(&json!({
                "max_tokens": 64,
                "messages": [{
                    "content": "Integration token limit test.",
                    "role": "user"
                }],
                "model": "claude-integration-test",
                "stream": false
            }))
            .send()
            .await?)
    }
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

fn now() -> Result<u64, FixtureError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn daily_usage_window(epoch_seconds: u64) -> String {
    format!("daily#{}", epoch_seconds / DAILY_WINDOW_SECONDS)
}

fn weekly_usage_window(epoch_seconds: u64) -> String {
    format!(
        "weekly#{}",
        epoch_seconds.saturating_add(MONDAY_WEEK_OFFSET_SECONDS) / WEEKLY_WINDOW_SECONDS
    )
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
