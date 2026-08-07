use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::io;
use std::panic::{AssertUnwindSafe, resume_unwind};
use std::time::Duration;

use aws_config::SdkConfig;
use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use futures_util::FutureExt;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use crate::IntegrationConfig;

pub type FixtureError = Box<dyn Error + Send + Sync>;
type HmacSha256 = Hmac<Sha256>;
const INDEX_PROPAGATION_TIMEOUT: Duration = Duration::from_secs(10);
const INITIAL_INDEX_POLL_DELAY: Duration = Duration::from_millis(50);
const MAX_INDEX_POLL_DELAY: Duration = Duration::from_millis(500);

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
    rate_limit_table_name: String,
    token_usage_table_name: String,
    user_id: String,
}

#[derive(Clone)]
pub struct EngineerFixtureContext {
    api_key: String,
    user_id: String,
}

impl EngineerFixtureContext {
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }
}

pub async fn with_engineer_fixture<T, F, Fut>(
    config: &IntegrationConfig,
    sdk_config: &SdkConfig,
    options: EngineerFixtureOptions,
    test: F,
) -> Result<T, FixtureError>
where
    F: FnOnce(EngineerFixtureContext) -> Fut,
    Fut: Future<Output = Result<T, FixtureError>>,
{
    let fixture = EngineerFixture::create(config, sdk_config, options).await?;
    let context = EngineerFixtureContext {
        api_key: fixture.api_key.clone(),
        user_id: fixture.user_id.clone(),
    };

    run_scoped(test(context), || fixture.cleanup()).await
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

        if let Err(propagation_error) = wait_for_api_key_index(
            &dynamodb_client,
            &config.engineers_table_name,
            &config.engineers_api_key_index_name,
            &api_key_hash,
        )
        .await
        {
            let cleanup_result =
                delete_engineer(&dynamodb_client, &config.engineers_table_name, &user_id).await;

            return match cleanup_result {
                Ok(()) => Err(propagation_error),
                Err(cleanup_error) => Err(io::Error::other(format!(
                    "API key index did not become ready: {propagation_error}; fixture cleanup failed: {cleanup_error}"
                ))
                .into()),
            };
        }

        Ok(Self {
            api_key,
            dynamodb_client,
            engineers_table_name: config.engineers_table_name.clone(),
            rate_limit_table_name: config.rate_limit_table_name.clone(),
            token_usage_table_name: config.token_usage_table_name.clone(),
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
        let mut errors = Vec::new();

        if let Err(error) = delete_partition_rows(
            &self.dynamodb_client,
            &self.rate_limit_table_name,
            "request_ts",
            &self.user_id,
        )
        .await
        {
            errors.push(format!("rate-limit cleanup failed: {error}"));
        }

        if let Err(error) = delete_partition_rows(
            &self.dynamodb_client,
            &self.token_usage_table_name,
            "usage_window",
            &self.user_id,
        )
        .await
        {
            errors.push(format!("token-usage cleanup failed: {error}"));
        }

        if let Err(error) = delete_engineer(
            &self.dynamodb_client,
            &self.engineers_table_name,
            &self.user_id,
        )
        .await
        {
            errors.push(format!("engineer cleanup failed: {error}"));
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")).into())
        }
    }
}

pub(super) async fn run_scoped<T, TestFuture, Cleanup, CleanupFuture>(
    test: TestFuture,
    cleanup: Cleanup,
) -> Result<T, FixtureError>
where
    TestFuture: Future<Output = Result<T, FixtureError>>,
    Cleanup: FnOnce() -> CleanupFuture,
    CleanupFuture: Future<Output = Result<(), FixtureError>>,
{
    let test_result = AssertUnwindSafe(test).catch_unwind().await;
    let cleanup_result = cleanup().await;

    match (test_result, cleanup_result) {
        (Ok(result), Ok(())) => result,
        (Ok(Ok(_)), Err(cleanup_error)) => Err(cleanup_error),
        (Ok(Err(test_error)), Err(cleanup_error)) => Err(io::Error::other(format!(
            "test failed: {test_error}; fixture cleanup failed: {cleanup_error}"
        ))
        .into()),
        (Err(panic), cleanup_result) => {
            if let Err(cleanup_error) = cleanup_result {
                eprintln!("fixture cleanup failed after test panic: {cleanup_error}");
            }
            resume_unwind(panic)
        }
    }
}

async fn delete_partition_rows(
    client: &DynamoDbClient,
    table_name: &str,
    sort_key_name: &str,
    user_id: &str,
) -> Result<(), FixtureError> {
    let mut exclusive_start_key = None;

    loop {
        let response = client
            .query()
            .table_name(table_name)
            .key_condition_expression("#user_id = :user_id")
            .expression_attribute_names("#user_id", "user_id")
            .expression_attribute_values(":user_id", AttributeValue::S(user_id.to_string()))
            .projection_expression("#user_id, #sort_key")
            .expression_attribute_names("#sort_key", sort_key_name)
            .set_exclusive_start_key(exclusive_start_key)
            .consistent_read(true)
            .send()
            .await?;

        for item in response.items() {
            let sort_key = item.get(sort_key_name).cloned().ok_or_else(|| {
                io::Error::other(format!(
                    "{table_name} cleanup row is missing sort key {sort_key_name}"
                ))
            })?;

            client
                .delete_item()
                .table_name(table_name)
                .key("user_id", AttributeValue::S(user_id.to_string()))
                .key(sort_key_name, sort_key)
                .send()
                .await?;
        }

        exclusive_start_key = response
            .last_evaluated_key()
            .filter(|key| !key.is_empty())
            .cloned();
        if exclusive_start_key.is_none() {
            return Ok(());
        }
    }
}

async fn wait_for_api_key_index(
    client: &DynamoDbClient,
    table_name: &str,
    index_name: &str,
    api_key_hash: &str,
) -> Result<(), FixtureError> {
    timeout(INDEX_PROPAGATION_TIMEOUT, async {
        let mut poll_delay = INITIAL_INDEX_POLL_DELAY;

        loop {
            let response = client
                .query()
                .table_name(table_name)
                .index_name(index_name)
                .key_condition_expression("#api_key_hash = :api_key_hash")
                .expression_attribute_names("#api_key_hash", "api_key_hash")
                .expression_attribute_values(
                    ":api_key_hash",
                    AttributeValue::S(api_key_hash.to_string()),
                )
                .limit(1)
                .send()
                .await?;

            if !response.items().is_empty() {
                return Ok(());
            }

            sleep(poll_delay).await;
            poll_delay = (poll_delay * 2).min(MAX_INDEX_POLL_DELAY);
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "API key hash was not visible in index {index_name} within {INDEX_PROPAGATION_TIMEOUT:?}"
            ),
        )
    })?
}

async fn delete_engineer(
    client: &DynamoDbClient,
    table_name: &str,
    user_id: &str,
) -> Result<(), FixtureError> {
    client
        .delete_item()
        .table_name(table_name)
        .key("user_id", AttributeValue::S(user_id.to_string()))
        .send()
        .await?;

    Ok(())
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
