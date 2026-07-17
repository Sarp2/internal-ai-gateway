use crate::token_reconciliation::TokenReconciliationJob;

#[test]
fn serializes_provider_neutral_reservation_jobs() {
    let job = TokenReconciliationJob::Reservation {
        actual_tokens: Some(4_200),
        completion_token: "completion-1".to_string(),
        daily_window: "daily#1".to_string(),
        reservation_window: "reservation#1".to_string(),
        token_budget: 7_000,
        user_id: "engineer-1".to_string(),
        weekly_window: "weekly#1".to_string(),
    };

    let serialized = serde_json::to_string(&job).expect("job should serialize");
    let parsed: TokenReconciliationJob =
        serde_json::from_str(&serialized).expect("job should deserialize");

    assert_eq!(parsed, job);
    assert!(!serialized.contains("openai"));
    assert!(!serialized.contains("anthropic"));
}

#[test]
fn serializes_usage_jobs_with_original_request_time() {
    let job = TokenReconciliationJob::Usage {
        job_id: "usage-1".to_string(),
        occurred_at: 1_700_000_000,
        token_count: 125,
        user_id: "engineer-1".to_string(),
    };

    let serialized = serde_json::to_string(&job).expect("job should serialize");
    let parsed: TokenReconciliationJob =
        serde_json::from_str(&serialized).expect("job should deserialize");

    assert_eq!(parsed, job);
}
