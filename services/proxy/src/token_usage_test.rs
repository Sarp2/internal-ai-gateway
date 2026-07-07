use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::config::BehaviorVersion;
use aws_sdk_dynamodb::types::AttributeValue;

use crate::engineer_auth::AuthenticatedEngineer;
use crate::token_usage::TokenUsageChecker;
use crate::token_usage::TokenUsageError;
use crate::token_usage::daily_usage_window;
use crate::token_usage::daily_usage_window_start;
use crate::token_usage::token_count_from_attribute;
use crate::token_usage::token_usage_ttl_epoch_seconds;
use crate::token_usage::weekly_usage_window;
use crate::token_usage::weekly_usage_window_start;

#[test]
fn builds_daily_usage_window_from_epoch_seconds() {
    assert_eq!(daily_usage_window(172_800), "daily#2");
}

#[test]
fn calculates_daily_usage_window_start() {
    assert_eq!(daily_usage_window_start(172_801), 172_800);
}

#[test]
fn builds_weekly_usage_window_from_monday_utc_boundary() {
    assert_eq!(weekly_usage_window(345_599), "weekly#0");
    assert_eq!(weekly_usage_window(345_600), "weekly#1");
}

#[test]
fn calculates_weekly_usage_window_start_from_monday_utc_boundary() {
    assert_eq!(weekly_usage_window_start(345_599), 0);
    assert_eq!(weekly_usage_window_start(345_600), 345_600);
    assert_eq!(weekly_usage_window_start(345_601), 345_600);
}

#[test]
fn keeps_token_usage_records_for_two_windows() {
    assert_eq!(token_usage_ttl_epoch_seconds(86_400, 86_400), 259_200);
}

#[test]
fn saturates_token_usage_ttl_for_extreme_values() {
    assert_eq!(
        token_usage_ttl_epoch_seconds(u64::MAX - 1, u64::MAX),
        u64::MAX
    );
}

#[test]
fn parses_token_count_attribute() {
    assert_eq!(
        token_count_from_attribute(&AttributeValue::N("123".to_string()))
            .expect("token count should parse"),
        123
    );
}

#[test]
fn rejects_invalid_token_count_attribute() {
    let error = token_count_from_attribute(&AttributeValue::S("123".to_string()))
        .expect_err("token count should be invalid");

    assert_eq!(error.to_string(), "token usage item is invalid");
}

#[tokio::test]
async fn allows_engineer_without_token_limits() {
    let checker = TokenUsageChecker::new(
        DynamoDbClient::from_conf(
            aws_sdk_dynamodb::Config::builder()
                .behavior_version(BehaviorVersion::latest())
                .build(),
        ),
        "token-usage",
    );

    let decision = checker
        .check_limits(&AuthenticatedEngineer {
            daily_token_limit: None,
            enabled: true,
            user_id: "user-123".to_string(),
            weekly_token_limit: None,
        })
        .await
        .expect("engineer without limits should be allowed");

    assert_eq!(decision.daily_tokens, 0);
    assert_eq!(decision.weekly_tokens, 0);
}

#[test]
fn describes_daily_limit_exceeded() {
    assert_eq!(
        TokenUsageError::DailyLimitExceeded.to_string(),
        "daily token limit exceeded"
    );
}

#[test]
fn describes_weekly_limit_exceeded() {
    assert_eq!(
        TokenUsageError::WeeklyLimitExceeded.to_string(),
        "weekly token limit exceeded"
    );
}

#[test]
fn describes_limit_exceeded_during_input_recording() {
    assert_eq!(
        TokenUsageError::LimitExceededDuringInputRecording.to_string(),
        "token limit exceeded while recording input tokens"
    );
}
