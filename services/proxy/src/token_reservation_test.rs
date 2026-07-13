use crate::token_reservation::reconciliation_values;

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
