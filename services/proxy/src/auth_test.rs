use axum::http::{HeaderMap, HeaderValue};

use crate::auth::read_api_key;

#[test]
fn reads_api_key_from_header() {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", HeaderValue::from_static("iag_test_key"));

    assert_eq!(
        read_api_key(&headers).expect("api key should be valid"),
        "iag_test_key"
    );
}

#[test]
fn rejects_missing_api_key_header() {
    let error = read_api_key(&HeaderMap::new()).expect_err("api key should be required");

    assert_eq!(error.to_string(), "missing api key");
}

#[test]
fn rejects_api_key_without_expected_prefix() {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", HeaderValue::from_static("other_test_key"));

    let error = read_api_key(&headers).expect_err("api key should be invalid");

    assert_eq!(error.to_string(), "invalid api key format");
}

#[test]
fn rejects_api_key_with_whitespace() {
    let mut headers = HeaderMap::new();
    headers.insert("x-api-key", HeaderValue::from_static("iag_test key"));

    let error = read_api_key(&headers).expect_err("api key should be invalid");

    assert_eq!(error.to_string(), "invalid api key format");
}
