use aws_sdk_dynamodb::types::CancellationReason;

use crate::token_reservation::{
    ReservationFailure, classify_cancellation_reasons, reconciliation_values,
};

fn cancellation_reason(code: &str) -> CancellationReason {
    CancellationReason::builder().code(code).build()
}

fn no_cancellation_reason() -> CancellationReason {
    CancellationReason::builder().build()
}

#[test]
fn classifies_daily_quota_condition_as_limit_exceeded() {
    let reasons = [
        cancellation_reason("ConditionalCheckFailed"),
        no_cancellation_reason(),
        no_cancellation_reason(),
    ];

    assert_eq!(
        classify_cancellation_reasons(&reasons),
        ReservationFailure::LimitExceeded
    );
}

#[test]
fn classifies_weekly_quota_condition_as_limit_exceeded() {
    let reasons = [
        no_cancellation_reason(),
        cancellation_reason("ConditionalCheckFailed"),
        no_cancellation_reason(),
    ];

    assert_eq!(
        classify_cancellation_reasons(&reasons),
        ReservationFailure::LimitExceeded
    );
}

#[test]
fn retries_transaction_conflicts_and_capacity_failures() {
    for code in [
        "TransactionConflict",
        "ProvisionedThroughputExceeded",
        "ThrottlingError",
    ] {
        let reasons = [cancellation_reason(code)];

        assert_eq!(
            classify_cancellation_reasons(&reasons),
            ReservationFailure::Retry
        );
    }
}

#[test]
fn does_not_describe_reservation_record_conflict_as_a_quota_failure() {
    let reasons = [
        no_cancellation_reason(),
        no_cancellation_reason(),
        cancellation_reason("ConditionalCheckFailed"),
    ];

    assert_eq!(
        classify_cancellation_reasons(&reasons),
        ReservationFailure::WriteFailed
    );
}

#[test]
fn treats_missing_and_unknown_cancellation_reasons_as_write_failures() {
    assert_eq!(
        classify_cancellation_reasons(&[]),
        ReservationFailure::WriteFailed
    );
    assert_eq!(
        classify_cancellation_reasons(&[cancellation_reason("ValidationError")]),
        ReservationFailure::WriteFailed
    );
}

#[test]
fn refunds_unused_reserved_tokens_after_exact_usage_arrives() {
    let (token_adjustment, reserved_adjustment, consumed_tokens) =
        reconciliation_values(7_000, Some(4_200));

    assert_eq!(token_adjustment, -2_800);
    assert_eq!(reserved_adjustment, -7_000);
    assert_eq!(consumed_tokens, 4_200);
}

#[test]
fn records_usage_above_the_original_reservation() {
    let (token_adjustment, reserved_adjustment, consumed_tokens) =
        reconciliation_values(7_000, Some(7_500));

    assert_eq!(token_adjustment, 500);
    assert_eq!(reserved_adjustment, -7_000);
    assert_eq!(consumed_tokens, 7_500);
}

#[test]
fn charges_the_full_reservation_when_exact_usage_is_missing() {
    let (token_adjustment, reserved_adjustment, consumed_tokens) =
        reconciliation_values(7_000, None);

    assert_eq!(token_adjustment, 0);
    assert_eq!(reserved_adjustment, -7_000);
    assert_eq!(consumed_tokens, 7_000);
}
