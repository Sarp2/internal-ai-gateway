use aws_sdk_dynamodb::types::AttributeValue;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{io, panic::AssertUnwindSafe};

use futures_util::FutureExt;

use super::engineer::{EngineerFixtureOptions, engineer_item, hash_api_key, run_scoped};

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

#[tokio::test]
async fn scoped_fixture_cleans_up_after_an_early_error() {
    let cleaned_up = Arc::new(AtomicBool::new(false));
    let cleanup_observer = Arc::clone(&cleaned_up);

    let result = run_scoped(
        async { Err::<(), _>(io::Error::other("test failed").into()) },
        move || async move {
            cleanup_observer.store(true, Ordering::SeqCst);
            Ok(())
        },
    )
    .await;

    assert!(result.is_err());
    assert!(cleaned_up.load(Ordering::SeqCst));
}

#[tokio::test]
async fn scoped_fixture_cleans_up_before_resuming_a_panic() {
    let cleaned_up = Arc::new(AtomicBool::new(false));
    let cleanup_observer = Arc::clone(&cleaned_up);

    let result = AssertUnwindSafe(run_scoped(panicking_test(), move || async move {
        cleanup_observer.store(true, Ordering::SeqCst);
        Ok(())
    }))
    .catch_unwind()
    .await;

    assert!(result.is_err());
    assert!(cleaned_up.load(Ordering::SeqCst));
}

async fn panicking_test() -> Result<(), super::engineer::FixtureError> {
    panic!("test panic");
}
