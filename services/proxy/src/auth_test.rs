use crate::auth::ApiKeyHasher;

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
