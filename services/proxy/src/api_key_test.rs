use crate::api_key::ApiKeyHasher;
use crate::api_key::ApiKeySecretError;

#[derive(Debug)]
struct TestSourceError;

impl std::fmt::Display for TestSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "source failure")
    }
}

impl std::error::Error for TestSourceError {}

#[test]
fn hashes_api_keys_consistently() {
    let hasher = ApiKeyHasher::new("test-secret");

    assert_eq!(
        hasher.hash_api_key("iag_test_key"),
        hasher.hash_api_key("iag_test_key")
    );
}

#[test]
fn hash_changes_when_api_key_changes() {
    let hasher = ApiKeyHasher::new("test-secret");

    assert_ne!(
        hasher.hash_api_key("iag_test_key"),
        hasher.hash_api_key("iag_other_key")
    );
}

#[test]
fn hash_changes_when_secret_changes() {
    let first_hasher = ApiKeyHasher::new("first-secret");
    let second_hasher = ApiKeyHasher::new("second-secret");

    assert_ne!(
        first_hasher.hash_api_key("iag_test_key"),
        second_hasher.hash_api_key("iag_test_key")
    );
}

#[test]
fn exposes_secret_fetch_failure_source() {
    let error = ApiKeySecretError::FetchFailed {
        source: Box::new(TestSourceError),
    };

    assert_eq!(
        std::error::Error::source(&error)
            .expect("source should exist")
            .to_string(),
        "source failure"
    );
}
