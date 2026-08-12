use std::time::Duration;

use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_sdk_dynamodb::Client as DynamoDbClient;
use reqwest::{Client, Response};
use serde_json::json;
use url::Url;

use crate::IntegrationConfig;
use crate::fixtures::engineer::FixtureError;

pub struct AnthropicMessagesEnvironment {
    client: Client,
    messages_url: Url,
    pub config: IntegrationConfig,
    pub dynamodb_client: DynamoDbClient,
    pub sdk_config: SdkConfig,
}

impl AnthropicMessagesEnvironment {
    pub async fn load() -> Result<Self, FixtureError> {
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
            messages_url,
            config,
            dynamodb_client,
            sdk_config,
        })
    }

    pub async fn send_messages(&self, api_key: &str) -> Result<Response, FixtureError> {
        self.send_messages_with_options(api_key, false, &[]).await
    }

    pub async fn send_messages_with_options(
        &self,
        api_key: &str,
        stream: bool,
        headers: &[(&str, &str)],
    ) -> Result<Response, FixtureError> {
        let mut request = self
            .client
            .post(self.messages_url.clone())
            .header("x-api-key", api_key)
            .json(&json!({
                "max_tokens": 64,
                "messages": [{
                    "content": "Integration proxy test.",
                    "role": "user"
                }],
                "model": "claude-integration-test",
                "stream": stream
            }));
        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        Ok(request.send().await?)
    }
}
