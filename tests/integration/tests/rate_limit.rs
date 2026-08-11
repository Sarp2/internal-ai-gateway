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
use tokio::time::sleep;
use url::Url;

#[tokio::test]
async fn allows_final_request_and_rejects_request_above_limit() -> Result<(), FixtureError> {
    let environment = RateLimitEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        EngineerFixtureOptions::default(),
        |engineer| async move {
            test_environment
                .seed_current_window(
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
    let environment = RateLimitEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        EngineerFixtureOptions::default(),
        |limited_engineer| async move {
            test_environment
                .seed_current_window(
                    limited_engineer.user_id(),
                    test_environment.config.rate_limit_requests_per_window,
                )
                .await?;

            with_engineer_fixture(
                &test_environment.config,
                &test_environment.sdk_config,
                EngineerFixtureOptions::default(),
                |other_engineer| async move {
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
    let environment = RateLimitEnvironment::load().await?;
    let test_environment = &environment;

    with_engineer_fixture(
        &environment.config,
        &environment.sdk_config,
        EngineerFixtureOptions::default(),
        |engineer| async move {
            let window_seconds = test_environment.config.rate_limit_window_seconds;
            let current_window = stable_current_window(window_seconds).await?;
            test_environment
                .seed_window(
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

struct RateLimitEnvironment {
    client: Client,
    config: IntegrationConfig,
    dynamodb_client: DynamoDbClient,
    messages_url: Url,
    sdk_config: SdkConfig,
}

impl RateLimitEnvironment {
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

    async fn seed_current_window(
        &self,
        user_id: &str,
        request_count: u64,
    ) -> Result<(), FixtureError> {
        self.seed_window(
            user_id,
            stable_current_window(self.config.rate_limit_window_seconds).await?,
            request_count,
        )
        .await
    }

    async fn seed_window(
        &self,
        user_id: &str,
        window_start: u64,
        request_count: u64,
    ) -> Result<(), FixtureError> {
        self.dynamodb_client
            .put_item()
            .table_name(&self.config.rate_limit_table_name)
            .item("user_id", AttributeValue::S(user_id.to_string()))
            .item("request_ts", AttributeValue::N(window_start.to_string()))
            .item(
                "request_count",
                AttributeValue::N(request_count.to_string()),
            )
            .item(
                "ttl",
                AttributeValue::N(
                    (window_start + self.config.rate_limit_window_seconds * 2).to_string(),
                ),
            )
            .send()
            .await?;
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
                    "content": "Integration rate limit test.",
                    "role": "user"
                }],
                "model": "claude-integration-test",
                "stream": false
            }))
            .send()
            .await?)
    }
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
