use std::time::Duration;

use aws_config::{BehaviorVersion, Region, SdkConfig};
use integration_tests::IntegrationConfig;
use integration_tests::fixtures::engineer::{
    EngineerFixtureOptions, FixtureError, with_engineer_fixture,
};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn rejects_request_without_api_key() -> Result<(), FixtureError> {
    let environment = AuthenticationEnvironment::load().await?;

    let response = environment.send_messages(None).await?;

    assert_auth_error(response, "missing api key").await
}

#[tokio::test]
async fn rejects_malformed_api_key() -> Result<(), FixtureError> {
    let environment = AuthenticationEnvironment::load().await?;

    let response = environment.send_messages(Some("invalid-api-key")).await?;

    assert_auth_error(response, "invalid api key format").await
}

#[tokio::test]
async fn rejects_unknown_api_key() -> Result<(), FixtureError> {
    let environment = AuthenticationEnvironment::load().await?;
    let api_key = format!("iag_unknown_{}", Uuid::new_v4().simple());

    let response = environment.send_messages(Some(&api_key)).await?;

    assert_auth_error(response, "invalid api key").await
}

#[tokio::test]
async fn rejects_disabled_engineer() -> Result<(), FixtureError> {
    let environment = AuthenticationEnvironment::load().await?;
    let config = &environment.config;
    let sdk_config = &environment.sdk_config;
    let client = environment.client.clone();
    let messages_url = environment.messages_url.clone();

    with_engineer_fixture(
        config,
        sdk_config,
        EngineerFixtureOptions {
            enabled: false,
            ..EngineerFixtureOptions::default()
        },
        |engineer| async move {
            let response = send_messages(&client, &messages_url, Some(engineer.api_key())).await?;

            assert_auth_error(response, "engineer is disabled").await
        },
    )
    .await
}

#[tokio::test]
async fn authenticates_enabled_engineer_and_reaches_provider() -> Result<(), FixtureError> {
    let environment = AuthenticationEnvironment::load().await?;
    let config = &environment.config;
    let sdk_config = &environment.sdk_config;
    let client = environment.client.clone();
    let messages_url = environment.messages_url.clone();

    with_engineer_fixture(
        config,
        sdk_config,
        EngineerFixtureOptions::default(),
        |engineer| async move {
            let response = send_messages(&client, &messages_url, Some(engineer.api_key())).await?;

            assert_eq!(response.status(), StatusCode::OK);
            let body = response.json::<Value>().await?;
            assert_eq!(body["type"], "message");
            assert_eq!(body["content"][0]["text"], "Mock Anthropic response.");
            Ok(())
        },
    )
    .await
}

struct AuthenticationEnvironment {
    client: Client,
    config: IntegrationConfig,
    messages_url: Url,
    sdk_config: SdkConfig,
}

impl AuthenticationEnvironment {
    async fn load() -> Result<Self, FixtureError> {
        let config = IntegrationConfig::load().map_err(std::io::Error::other)?;
        let sdk_config = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(config.aws_region.clone()))
            .load()
            .await;
        let mut messages_url = Url::parse(&config.proxy_health_url)?;
        messages_url.set_path("/v1/anthropic/messages");
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;

        Ok(Self {
            client,
            config,
            messages_url,
            sdk_config,
        })
    }

    async fn send_messages(&self, api_key: Option<&str>) -> Result<Response, FixtureError> {
        send_messages(&self.client, &self.messages_url, api_key).await
    }
}

async fn send_messages(
    client: &Client,
    messages_url: &Url,
    api_key: Option<&str>,
) -> Result<Response, FixtureError> {
    let mut request = client.post(messages_url.clone()).json(&json!({
        "max_tokens": 64,
        "messages": [{
            "content": "Integration authentication test.",
            "role": "user"
        }],
        "model": "claude-integration-test",
        "stream": false
    }));

    if let Some(api_key) = api_key {
        request = request.header("x-api-key", api_key);
    }

    Ok(request.send().await?)
}

async fn assert_auth_error(response: Response, expected_message: &str) -> Result<(), FixtureError> {
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = response.json::<Value>().await?;
    assert_eq!(body, json!({ "message": expected_message }));
    Ok(())
}
