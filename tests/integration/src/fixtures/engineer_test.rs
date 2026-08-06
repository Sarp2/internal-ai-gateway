use aws_sdk_dynamodb::types::AttributeValue;

use super::engineer::{EngineerFixtureOptions, engineer_item, hash_api_key};

#[test]
fn builds_enabled_engineer_with_optional_token_limits() {
    let item = engineer_item(
        "engineer-1",
        "api-key-hash",
        &EngineerFixtureOptions {
            daily_token_limit: Some(1_000),
            enabled: true,
            weekly_token_limit: Some(7_000),
        },
    );

    assert_eq!(
        item.get("user_id"),
        Some(&AttributeValue::S("engineer-1".to_string()))
    );
    assert_eq!(item.get("enabled"), Some(&AttributeValue::Bool(true)));
    assert_eq!(
        item.get("daily_token_limit"),
        Some(&AttributeValue::N("1000".to_string()))
    );
    assert_eq!(
        item.get("weekly_token_limit"),
        Some(&AttributeValue::N("7000".to_string()))
    );
}

#[test]
fn hashes_api_keys_with_the_production_algorithm() {
    assert_eq!(
        hash_api_key(b"test-secret", "iag_test_key"),
        "872fa2d4afd8760a6b2b5b314f91db74d7ddd767dd7c365b090cf280da8a2be3"
    );
}
