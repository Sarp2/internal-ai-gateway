use std::time::Duration;

use crate::config::ProxyConfig;

#[test]
fn uses_defaults_when_env_values_are_missing() {
    let config = ProxyConfig::from_values(|_| None);

    assert_eq!(config.port, 8080);
    assert_eq!(config.max_active_streams, 200);
    assert_eq!(config.metric_interval, Duration::from_secs(15));
}

#[test]
fn parses_env_values() {
    let config = ProxyConfig::from_values(|name| match name {
        "PORT" => Some("9090".to_string()),
        "MAX_ACTIVE_STREAMS" => Some("250".to_string()),
        "ACTIVE_STREAM_METRIC_INTERVAL_SECONDS" => Some("30".to_string()),
        _ => None,
    });

    assert_eq!(config.port, 9090);
    assert_eq!(config.max_active_streams, 250);
    assert_eq!(config.metric_interval, Duration::from_secs(30));
}

#[test]
fn falls_back_to_defaults_for_invalid_env_values() {
    let config = ProxyConfig::from_values(|_| Some("invalid".to_string()));

    assert_eq!(config.port, 8080);
    assert_eq!(config.max_active_streams, 200);
    assert_eq!(config.metric_interval, Duration::from_secs(15));
}
