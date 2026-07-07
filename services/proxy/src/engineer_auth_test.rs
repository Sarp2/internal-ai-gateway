use std::collections::HashMap;

use aws_sdk_dynamodb::types::AttributeValue;

use crate::engineer_auth::AuthenticatedEngineer;
use crate::engineer_auth::EngineerAuthError;
use crate::engineer_auth::authenticated_engineer_from_item;

#[test]
fn maps_dynamodb_item_to_authenticated_engineer() {
    let item = HashMap::from([
        ("enabled".to_string(), AttributeValue::Bool(true)),
        (
            "user_id".to_string(),
            AttributeValue::S("user-123".to_string()),
        ),
    ]);

    assert_eq!(
        authenticated_engineer_from_item(&item).expect("engineer should map"),
        AuthenticatedEngineer {
            daily_token_limit: None,
            enabled: true,
            user_id: "user-123".to_string(),
            weekly_token_limit: None,
        }
    );
}

#[test]
fn maps_optional_token_limits_to_authenticated_engineer() {
    let item = HashMap::from([
        (
            "daily_token_limit".to_string(),
            AttributeValue::N("1000".to_string()),
        ),
        ("enabled".to_string(), AttributeValue::Bool(true)),
        (
            "user_id".to_string(),
            AttributeValue::S("user-123".to_string()),
        ),
        (
            "weekly_token_limit".to_string(),
            AttributeValue::N("7000".to_string()),
        ),
    ]);

    assert_eq!(
        authenticated_engineer_from_item(&item).expect("engineer should map"),
        AuthenticatedEngineer {
            daily_token_limit: Some(1000),
            enabled: true,
            user_id: "user-123".to_string(),
            weekly_token_limit: Some(7000),
        }
    );
}

#[test]
fn rejects_item_without_user_id() {
    let item = HashMap::from([("enabled".to_string(), AttributeValue::Bool(true))]);

    let error = authenticated_engineer_from_item(&item).expect_err("item should be invalid");

    assert_eq!(
        error.to_string(),
        "engineer auth item is missing valid user_id"
    );
}

#[test]
fn rejects_item_with_empty_user_id() {
    let item = HashMap::from([
        ("enabled".to_string(), AttributeValue::Bool(true)),
        ("user_id".to_string(), AttributeValue::S(String::new())),
    ]);

    let error = authenticated_engineer_from_item(&item).expect_err("item should be invalid");

    assert_eq!(
        error.to_string(),
        "engineer auth item is missing valid user_id"
    );
}

#[test]
fn rejects_item_without_enabled_flag() {
    let item = HashMap::from([(
        "user_id".to_string(),
        AttributeValue::S("user-123".to_string()),
    )]);

    let error = authenticated_engineer_from_item(&item).expect_err("item should be invalid");

    assert_eq!(
        error.to_string(),
        "engineer auth item is missing valid enabled"
    );
}

#[test]
fn describes_duplicate_api_key_hash_as_auth_failure() {
    assert_eq!(
        EngineerAuthError::DuplicateApiKeyHash.to_string(),
        "multiple engineers matched the same api key hash"
    );
}
