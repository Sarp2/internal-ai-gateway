use std::error::Error;
use std::time::Duration;

use aws_config::{BehaviorVersion, Region, SdkConfig};
use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::{KeySchemaElement, KeyType, ProjectionType, TableStatus};
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use aws_sdk_sqs::Client as SqsClient;
use aws_sdk_sqs::types::QueueAttributeName;
use integration_tests::IntegrationConfig;
use reqwest::StatusCode;
use serde_json::Value;

#[tokio::test]
async fn engineers_table_has_api_key_lookup_schema() -> Result<(), Box<dyn Error>> {
    let (config, sdk_config) = load_aws_config().await?;
    let client = DynamoDbClient::new(&sdk_config);
    assert_table(
        &client,
        &config.engineers_table_name,
        &[key("user_id", KeyType::Hash)],
    )
    .await?;

    let response = client
        .describe_table()
        .table_name(&config.engineers_table_name)
        .send()
        .await?;
    let api_key_index = response.table().and_then(|table| {
        table
            .global_secondary_indexes()
            .iter()
            .find(|index| index.index_name() == Some(config.engineers_api_key_index_name.as_str()))
    });

    let api_key_index = api_key_index.ok_or("engineers API key GSI should exist")?;

    assert_eq!(
        api_key_index.key_schema(),
        &[key("api_key_hash", KeyType::Hash)]
    );
    assert_eq!(
        api_key_index
            .projection()
            .and_then(|projection| projection.projection_type()),
        Some(&ProjectionType::All)
    );
    Ok(())
}

#[tokio::test]
async fn messages_table_has_message_ordering_schema() -> Result<(), Box<dyn Error>> {
    let (config, sdk_config) = load_aws_config().await?;
    assert_table(
        &DynamoDbClient::new(&sdk_config),
        &config.messages_table_name,
        &[
            key("user_id", KeyType::Hash),
            key("message_id", KeyType::Range),
        ],
    )
    .await
}

#[tokio::test]
async fn rate_limit_table_has_request_window_schema() -> Result<(), Box<dyn Error>> {
    let (config, sdk_config) = load_aws_config().await?;
    assert_table(
        &DynamoDbClient::new(&sdk_config),
        &config.rate_limit_table_name,
        &[
            key("user_id", KeyType::Hash),
            key("request_ts", KeyType::Range),
        ],
    )
    .await
}

#[tokio::test]
async fn token_usage_table_has_usage_window_schema() -> Result<(), Box<dyn Error>> {
    let (config, sdk_config) = load_aws_config().await?;
    assert_table(
        &DynamoDbClient::new(&sdk_config),
        &config.token_usage_table_name,
        &[
            key("user_id", KeyType::Hash),
            key("usage_window", KeyType::Range),
        ],
    )
    .await
}

#[tokio::test]
async fn reconciliation_queue_is_reachable() -> Result<(), Box<dyn Error>> {
    let (config, sdk_config) = load_aws_config().await?;
    assert_queue(
        &SqsClient::new(&sdk_config),
        &config.token_reconciliation_queue_url,
    )
    .await
}

#[tokio::test]
async fn reconciliation_dead_letter_queue_is_reachable() -> Result<(), Box<dyn Error>> {
    let (config, sdk_config) = load_aws_config().await?;
    assert_queue(
        &SqsClient::new(&sdk_config),
        &config.token_reconciliation_dead_letter_queue_url,
    )
    .await
}

#[tokio::test]
async fn proxy_api_key_hash_secret_is_reachable() -> Result<(), Box<dyn Error>> {
    let (config, sdk_config) = load_aws_config().await?;
    let secret = SecretsManagerClient::new(&sdk_config)
        .describe_secret()
        .secret_id(&config.proxy_api_key_hash_secret_arn)
        .send()
        .await?;

    assert_eq!(
        secret.arn(),
        Some(config.proxy_api_key_hash_secret_arn.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn proxy_health_endpoint_reports_healthy() -> Result<(), Box<dyn Error>> {
    let config = IntegrationConfig::load()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let response = client.get(&config.proxy_health_url).send().await?;

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.json::<Value>().await?;
    assert_eq!(body, serde_json::json!({ "status": "ok" }));
    Ok(())
}

async fn load_aws_config() -> Result<(IntegrationConfig, SdkConfig), Box<dyn Error>> {
    let config = IntegrationConfig::load()?;
    let sdk_config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(config.aws_region.clone()))
        .load()
        .await;
    Ok((config, sdk_config))
}

async fn assert_table(
    client: &DynamoDbClient,
    table_name: &str,
    expected_key_schema: &[KeySchemaElement],
) -> Result<(), Box<dyn Error>> {
    let response = client
        .describe_table()
        .table_name(table_name)
        .send()
        .await?;
    let table = response
        .table()
        .ok_or("DynamoDB table description is missing")?;

    assert_eq!(table.table_status(), Some(&TableStatus::Active));
    assert_eq!(table.key_schema(), expected_key_schema);
    Ok(())
}

async fn assert_queue(client: &SqsClient, queue_url: &str) -> Result<(), Box<dyn Error>> {
    let response = client
        .get_queue_attributes()
        .queue_url(queue_url)
        .attribute_names(QueueAttributeName::QueueArn)
        .send()
        .await?;
    let queue_arn = response
        .attributes()
        .and_then(|attributes| attributes.get(&QueueAttributeName::QueueArn))
        .ok_or("SQS queue ARN attribute is missing")?;

    assert!(!queue_arn.is_empty());
    Ok(())
}

fn key(name: &str, key_type: KeyType) -> KeySchemaElement {
    KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(key_type)
        .build()
        .expect("test key schema should be valid")
}
