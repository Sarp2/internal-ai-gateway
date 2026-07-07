use std::time::Duration;

use crate::config::ProxyConfig;

#[test]
fn uses_defaults_when_env_values_are_missing() {
    let config = ProxyConfig::from_values(test_value).expect("config should parse");

    assert_eq!(config.port, 8080);
    assert_eq!(config.max_active_streams, 200);
    assert_eq!(config.metric_interval, Duration::from_secs(15));
    assert_eq!(config.engineers_table_name, "engineers");
    assert_eq!(config.engineers_api_key_index_name, "ApiKeyIndex");
    assert_eq!(
        config.proxy_api_key_hash_secret_arn,
        "arn:aws:secretsmanager:proxy-api-key-hash"
    );
    assert_eq!(config.rate_limit_requests_per_window, 120);
    assert_eq!(config.rate_limit_table_name, "rate-limits");
    assert_eq!(config.rate_limit_window, Duration::from_secs(60));
    assert_eq!(config.token_usage_table_name, "token-usage");
}

#[test]
fn parses_env_values() {
    let config = ProxyConfig::from_values(|name| match name {
        "PORT" => Some("9090".to_string()),
        "MAX_ACTIVE_STREAMS" => Some("250".to_string()),
        "ACTIVE_STREAM_METRIC_INTERVAL_SECONDS" => Some("30".to_string()),
        "ENGINEERS_TABLE_NAME" => Some("custom-engineers".to_string()),
        "ENGINEERS_API_KEY_INDEX_NAME" => Some("CustomApiKeyIndex".to_string()),
        "PROXY_API_KEY_HASH_SECRET_ARN" => Some("arn:aws:secretsmanager:custom".to_string()),
        "RATE_LIMIT_REQUESTS_PER_WINDOW" => Some("250".to_string()),
        "RATE_LIMIT_TABLE_NAME" => Some("custom-rate-limits".to_string()),
        "RATE_LIMIT_WINDOW_SECONDS" => Some("120".to_string()),
        "TOKEN_USAGE_TABLE_NAME" => Some("custom-token-usage".to_string()),
        _ => None,
    })
    .expect("config should parse");

    assert_eq!(config.port, 9090);
    assert_eq!(config.max_active_streams, 250);
    assert_eq!(config.metric_interval, Duration::from_secs(30));
    assert_eq!(config.engineers_table_name, "custom-engineers");
    assert_eq!(config.engineers_api_key_index_name, "CustomApiKeyIndex");
    assert_eq!(
        config.proxy_api_key_hash_secret_arn,
        "arn:aws:secretsmanager:custom"
    );
    assert_eq!(config.rate_limit_requests_per_window, 250);
    assert_eq!(config.rate_limit_table_name, "custom-rate-limits");
    assert_eq!(config.rate_limit_window, Duration::from_secs(120));
    assert_eq!(config.token_usage_table_name, "custom-token-usage");
}

#[test]
fn falls_back_to_defaults_for_invalid_env_values() {
    let config = ProxyConfig::from_values(|name| match name {
        "ENGINEERS_TABLE_NAME" => Some("engineers".to_string()),
        "ENGINEERS_API_KEY_INDEX_NAME" => Some("ApiKeyIndex".to_string()),
        "PROXY_API_KEY_HASH_SECRET_ARN" => {
            Some("arn:aws:secretsmanager:proxy-api-key-hash".to_string())
        }
        "RATE_LIMIT_TABLE_NAME" => Some("rate-limits".to_string()),
        "TOKEN_USAGE_TABLE_NAME" => Some("token-usage".to_string()),
        _ => Some("invalid".to_string()),
    })
    .expect("config should parse");

    assert_eq!(config.port, 8080);
    assert_eq!(config.max_active_streams, 200);
    assert_eq!(config.metric_interval, Duration::from_secs(15));
    assert_eq!(config.rate_limit_requests_per_window, 120);
    assert_eq!(config.rate_limit_window, Duration::from_secs(60));
}

#[test]
fn clamps_zero_values_that_would_disable_runtime_safety() {
    let config = ProxyConfig::from_values(|name| match name {
        "MAX_ACTIVE_STREAMS"
        | "ACTIVE_STREAM_METRIC_INTERVAL_SECONDS"
        | "RATE_LIMIT_REQUESTS_PER_WINDOW"
        | "RATE_LIMIT_WINDOW_SECONDS" => Some("0".to_string()),
        "ENGINEERS_TABLE_NAME" => Some("engineers".to_string()),
        "ENGINEERS_API_KEY_INDEX_NAME" => Some("ApiKeyIndex".to_string()),
        "PROXY_API_KEY_HASH_SECRET_ARN" => {
            Some("arn:aws:secretsmanager:proxy-api-key-hash".to_string())
        }
        "RATE_LIMIT_TABLE_NAME" => Some("rate-limits".to_string()),
        "TOKEN_USAGE_TABLE_NAME" => Some("token-usage".to_string()),
        _ => None,
    })
    .expect("config should parse");

    assert_eq!(config.max_active_streams, 1);
    assert_eq!(config.metric_interval, Duration::from_secs(1));
    assert_eq!(config.rate_limit_requests_per_window, 1);
    assert_eq!(config.rate_limit_window, Duration::from_secs(1));
}

#[test]
fn rejects_missing_proxy_api_key_hash_secret_arn() {
    let error = ProxyConfig::from_values(|name| match name {
        "ENGINEERS_TABLE_NAME" => Some("engineers".to_string()),
        "ENGINEERS_API_KEY_INDEX_NAME" => Some("ApiKeyIndex".to_string()),
        "RATE_LIMIT_TABLE_NAME" => Some("rate-limits".to_string()),
        "TOKEN_USAGE_TABLE_NAME" => Some("token-usage".to_string()),
        _ => None,
    })
    .expect_err("config should fail");

    assert_eq!(
        error.to_string(),
        "missing required environment value PROXY_API_KEY_HASH_SECRET_ARN"
    );
}

fn test_value(name: &str) -> Option<String> {
    match name {
        "ENGINEERS_TABLE_NAME" => Some("engineers".to_string()),
        "ENGINEERS_API_KEY_INDEX_NAME" => Some("ApiKeyIndex".to_string()),
        "PROXY_API_KEY_HASH_SECRET_ARN" => {
            Some("arn:aws:secretsmanager:proxy-api-key-hash".to_string())
        }
        "RATE_LIMIT_TABLE_NAME" => Some("rate-limits".to_string()),
        "TOKEN_USAGE_TABLE_NAME" => Some("token-usage".to_string()),
        _ => None,
    }
}
