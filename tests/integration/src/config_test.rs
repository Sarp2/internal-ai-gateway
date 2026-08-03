use serde_json::json;

use super::{DYNAMODB_STACK, ECS_STACK, IntegrationConfig, RECONCILIATION_STACK, SECRETS_STACK};

#[test]
fn reads_required_stack_outputs() {
    let outputs = valid_outputs();

    let config = IntegrationConfig::from_outputs(&outputs).expect("outputs should be complete");

    assert_eq!(config.aws_region, "eu-north-1");
    assert_eq!(config.engineers_table_name, "engineers");
    assert_eq!(config.proxy_health_url, "http://proxy.example/health");
    assert_eq!(
        config.token_reconciliation_dead_letter_queue_url,
        "https://sqs.eu-north-1.amazonaws.com/123456789012/internal-ai-gateway-integration-token-reconciliation-dlq"
    );
}

#[test]
fn rejects_missing_stack_outputs() {
    let error = IntegrationConfig::from_outputs(&json!({}))
        .expect_err("missing outputs should be rejected");

    assert_eq!(
        error,
        "missing integration output InternalAiGatewayIntegrationEcsStack.AwsRegion"
    );
}

#[test]
fn rejects_outputs_with_invalid_resource_shapes() {
    let mut outputs = valid_outputs();
    outputs[ECS_STACK]["ProxyHealthUrl"] = json!("https://proxy.example/not-health");

    let error = IntegrationConfig::from_outputs(&outputs)
        .expect_err("invalid resource shapes should be rejected");

    assert_eq!(
        error,
        "invalid integration proxy health URL https://proxy.example/not-health"
    );
}

fn valid_outputs() -> serde_json::Value {
    json!({
        (DYNAMODB_STACK): {
            "EngineersApiKeyIndexName": "ApiKeyIndex",
            "EngineersTableName": "engineers",
            "MessagesTableName": "messages",
            "RateLimitTableName": "rate-limits",
            "TokenUsageTableName": "token-usage"
        },
        (ECS_STACK): {
            "AwsRegion": "eu-north-1",
            "ProxyHealthUrl": "http://proxy.example/health"
        },
        (RECONCILIATION_STACK): {
            "TokenReconciliationDeadLetterQueueUrl": "https://sqs.eu-north-1.amazonaws.com/123456789012/internal-ai-gateway-integration-token-reconciliation-dlq",
            "TokenReconciliationQueueUrl": "https://sqs.eu-north-1.amazonaws.com/123456789012/internal-ai-gateway-integration-token-reconciliation"
        },
        (SECRETS_STACK): {
            "ProxyApiKeyHashSecretArn": "arn:aws:secretsmanager:eu-north-1:123456789012:secret:internal-ai-gateway/integration/proxy-api-key-hash-abc123"
        }
    })
}
