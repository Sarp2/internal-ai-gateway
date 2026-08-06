use std::collections::HashMap;
use std::error::Error;
use std::io;

use aws_config::SdkConfig;
use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

use crate::IntegrationConfig;

type FixtureError = Box<dyn Error + Send + Sync>;
type HmacSha256 = Hmac<Sha256>;

pub struct EngineerFixtureOptions {
    pub daily_token_limit: Option<u64>,
    pub enabled: bool,
    pub weekly_token_limit: Option<u64>,
}

impl Default for EngineerFixtureOptions {
    fn default() -> Self {
        Self {
            daily_token_limit: None,
            enabled: true,
            weekly_token_limit: None,
        }
    }
}

pub struct EngineerFixture {
    api_key: String,
    dynamodb_client: DynamoDbClient,
    engineers_table_name: String,
    user_id: String,
}

impl EngineerFixture {
    pub async fn create(
        config: &IntegrationConfig,
        sdk_config: &SdkConfig,
        options: EngineerFixtureOptions,
    ) -> Result<Self, FixtureError> {
        let unique_id = Uuid::new_v4();
        let api_key = format!("iag_test_{}", unique_id.simple());
        let user_id = format!("integration-engineer-{unique_id}");
        let secret = load_hash_secret(
            &SecretsManagerClient::new(sdk_config),
            &config.proxy_api_key_hash_secret_arn,
        )
        .await?;

        let api_key_hash = hash_api_key(&secret, &api_key);
        let dynamodb_client = DynamoDbClient::new(sdk_config);

        dynamodb_client
            .put_item()
            .table_name(&config.engineers_table_name)
            .set_item(Some(engineer_item(&user_id, &api_key_hash, &options)))
            .condition_expression("attribute_not_exists(user_id)")
            .send()
            .await?;

        Ok(Self {
            api_key,
            dynamodb_client,
            engineers_table_name: config.engineers_table_name.clone(),
            user_id,
        })
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub async fn cleanup(self) -> Result<(), FixtureError> {
        self.dynamodb_client
            .delete_item()
            .table_name(self.engineers_table_name)
            .key("user_id", AttributeValue::S(self.user_id))
            .send()
            .await?;

        Ok(())
    }
}

async fn load_hash_secret(
    client: &SecretsManagerClient,
    secret_arn: &str,
) -> Result<Vec<u8>, FixtureError> {
    let response = client
        .get_secret_value()
        .secret_id(secret_arn)
        .send()
        .await?;
    let secret = response
        .secret_string()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::other("proxy API key hash secret must be a non-empty string"))?;

    Ok(secret.as_bytes().to_vec())
}

pub(super) fn hash_api_key(secret: &[u8], api_key: &str) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret).expect("HMAC-SHA256 accepts secret keys of any size");
    mac.update(api_key.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub(super) fn engineer_item(
    user_id: &str,
    api_key_hash: &str,
    options: &EngineerFixtureOptions,
) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::from([
        (
            "user_id".to_string(),
            AttributeValue::S(user_id.to_string()),
        ),
        (
            "api_key_hash".to_string(),
            AttributeValue::S(api_key_hash.to_string()),
        ),
        ("enabled".to_string(), AttributeValue::Bool(options.enabled)),
    ]);

    if let Some(limit) = options.daily_token_limit {
        item.insert(
            "daily_token_limit".to_string(),
            AttributeValue::N(limit.to_string()),
        );
    }
    if let Some(limit) = options.weekly_token_limit {
        item.insert(
            "weekly_token_limit".to_string(),
            AttributeValue::N(limit.to_string()),
        );
    }

    item
}
