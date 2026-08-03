use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use url::Url;

#[cfg(test)]
mod config_test;

const DEFAULT_OUTPUTS_FILE: &str = ".integration/cdk-outputs.json";
const DYNAMODB_STACK: &str = "InternalAiGatewayIntegrationDynamoDbStack";
const ECS_STACK: &str = "InternalAiGatewayIntegrationEcsStack";
const RECONCILIATION_STACK: &str = "InternalAiGatewayIntegrationReconciliationStack";
const SECRETS_STACK: &str = "InternalAiGatewayIntegrationSecretsStack";

#[derive(Debug, Eq, PartialEq)]
pub struct IntegrationConfig {
    pub aws_region: String,
    pub engineers_api_key_index_name: String,
    pub engineers_table_name: String,
    pub messages_table_name: String,
    pub proxy_api_key_hash_secret_arn: String,
    pub proxy_health_url: String,
    pub rate_limit_table_name: String,
    pub token_reconciliation_queue_url: String,
    pub token_reconciliation_dead_letter_queue_url: String,
    pub token_usage_table_name: String,
}

impl IntegrationConfig {
    pub fn load() -> Result<Self, String> {
        let outputs_file = std::env::var_os("INTEGRATION_OUTPUTS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_OUTPUTS_FILE));
        Self::from_outputs_file(&outputs_file)
    }

    pub fn from_outputs_file(path: &Path) -> Result<Self, String> {
        let contents = fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        let outputs: Value = serde_json::from_str(&contents)
            .map_err(|error| format!("failed to parse {}: {error}", path.display()))?;

        Self::from_outputs(&outputs)
    }

    fn from_outputs(outputs: &Value) -> Result<Self, String> {
        let config = Self {
            aws_region: output(outputs, ECS_STACK, "AwsRegion")?,
            engineers_api_key_index_name: output(
                outputs,
                DYNAMODB_STACK,
                "EngineersApiKeyIndexName",
            )?,
            engineers_table_name: output(outputs, DYNAMODB_STACK, "EngineersTableName")?,
            messages_table_name: output(outputs, DYNAMODB_STACK, "MessagesTableName")?,
            proxy_api_key_hash_secret_arn: output(
                outputs,
                SECRETS_STACK,
                "ProxyApiKeyHashSecretArn",
            )?,
            proxy_health_url: output(outputs, ECS_STACK, "ProxyHealthUrl")?,
            rate_limit_table_name: output(outputs, DYNAMODB_STACK, "RateLimitTableName")?,
            token_reconciliation_queue_url: output(
                outputs,
                RECONCILIATION_STACK,
                "TokenReconciliationQueueUrl",
            )?,
            token_reconciliation_dead_letter_queue_url: output(
                outputs,
                RECONCILIATION_STACK,
                "TokenReconciliationDeadLetterQueueUrl",
            )?,
            token_usage_table_name: output(outputs, DYNAMODB_STACK, "TokenUsageTableName")?,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_health_url(&self.proxy_health_url)?;
        validate_queue_url(
            &self.token_reconciliation_queue_url,
            &self.aws_region,
            "internal-ai-gateway-integration-token-reconciliation",
        )?;
        validate_queue_url(
            &self.token_reconciliation_dead_letter_queue_url,
            &self.aws_region,
            "internal-ai-gateway-integration-token-reconciliation-dlq",
        )?;
        if self.token_reconciliation_queue_url == self.token_reconciliation_dead_letter_queue_url {
            return Err("integration reconciliation queue and DLQ must be different".to_string());
        }
        validate_secret_arn(
            &self.proxy_api_key_hash_secret_arn,
            &self.aws_region,
            "internal-ai-gateway/integration/proxy-api-key-hash",
        )
    }
}

fn validate_health_url(value: &str) -> Result<(), String> {
    let url = parse_url(value, "proxy health URL")?;
    if url.scheme() != "http"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.path() != "/health"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!("invalid integration proxy health URL {value}"));
    }
    Ok(())
}

fn validate_queue_url(value: &str, region: &str, queue_name: &str) -> Result<(), String> {
    let url = parse_url(value, "SQS queue URL")?;
    let expected_host = format!("sqs.{region}.amazonaws.com");
    let segments = url
        .path_segments()
        .map(Iterator::collect::<Vec<_>>)
        .unwrap_or_default();
    let valid_account = segments.first().is_some_and(|account| {
        account.len() == 12 && account.chars().all(|value| value.is_ascii_digit())
    });
    if url.scheme() != "https"
        || url.host_str() != Some(expected_host.as_str())
        || segments.len() != 2
        || !valid_account
        || segments[1] != queue_name
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(format!("invalid integration SQS queue URL {value}"));
    }
    Ok(())
}

fn validate_secret_arn(value: &str, region: &str, secret_name: &str) -> Result<(), String> {
    let parts = value.splitn(6, ':').collect::<Vec<_>>();
    let valid_account = parts.get(4).is_some_and(|account| {
        account.len() == 12 && account.chars().all(|value| value.is_ascii_digit())
    });
    let valid_resource = parts
        .get(5)
        .is_some_and(|resource| resource.starts_with(&format!("secret:{secret_name}")));
    if parts.first() != Some(&"arn")
        || parts.get(1) != Some(&"aws")
        || parts.get(2) != Some(&"secretsmanager")
        || parts.get(3) != Some(&region)
        || !valid_account
        || !valid_resource
    {
        return Err(format!("invalid integration Secrets Manager ARN {value}"));
    }
    Ok(())
}

fn parse_url(value: &str, description: &str) -> Result<Url, String> {
    Url::parse(value).map_err(|error| format!("invalid integration {description} {value}: {error}"))
}

fn output(outputs: &Value, stack: &str, name: &str) -> Result<String, String> {
    outputs
        .get(stack)
        .and_then(|stack_outputs| stack_outputs.get(name))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("missing integration output {stack}.{name}"))
}
