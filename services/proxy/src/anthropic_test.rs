use axum::http::HeaderMap;
use axum::http::header::{CONNECTION, CONTENT_LENGTH, HOST, TRANSFER_ENCODING};

use crate::anthropic::{
    ConnectionHeaderNames, should_forward_request_header, should_forward_response_header,
    test_header,
};

#[test]
fn strips_internal_and_hop_by_hop_request_headers() {
    let connection_headers = ConnectionHeaderNames::default();

    assert!(!should_forward_request_header(
        &test_header("x-api-key"),
        &connection_headers
    ));
    assert!(!should_forward_request_header(&HOST, &connection_headers));
    assert!(!should_forward_request_header(
        &CONTENT_LENGTH,
        &connection_headers
    ));
    assert!(!should_forward_request_header(
        &CONNECTION,
        &connection_headers
    ));
    assert!(!should_forward_request_header(
        &TRANSFER_ENCODING,
        &connection_headers
    ));
}

#[test]
fn forwards_provider_request_headers() {
    let connection_headers = ConnectionHeaderNames::default();

    assert!(should_forward_request_header(
        &test_header("anthropic-version"),
        &connection_headers
    ));
    assert!(should_forward_request_header(
        &test_header("anthropic-beta"),
        &connection_headers
    ));
    assert!(should_forward_request_header(
        &test_header("content-type"),
        &connection_headers
    ));
}

#[test]
fn strips_connection_nominated_request_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(CONNECTION, "x-debug, x-extra-hop".parse().unwrap());
    let connection_headers = ConnectionHeaderNames::from_headers(&headers);

    assert!(!should_forward_request_header(
        &test_header("x-debug"),
        &connection_headers
    ));
    assert!(!should_forward_request_header(
        &test_header("x-extra-hop"),
        &connection_headers
    ));
}

#[test]
fn strips_streaming_response_headers_that_axum_recomputes() {
    let connection_headers = ConnectionHeaderNames::default();

    assert!(!should_forward_response_header(
        &CONTENT_LENGTH,
        &connection_headers
    ));
    assert!(!should_forward_response_header(
        &CONNECTION,
        &connection_headers
    ));
    assert!(!should_forward_response_header(
        &TRANSFER_ENCODING,
        &connection_headers
    ));
}

#[test]
fn forwards_provider_response_headers() {
    let connection_headers = ConnectionHeaderNames::default();

    assert!(should_forward_response_header(
        &test_header("content-type"),
        &connection_headers
    ));
    assert!(should_forward_response_header(
        &test_header("request-id"),
        &connection_headers
    ));
    assert!(should_forward_response_header(
        &test_header("anthropic-ratelimit-requests-limit"),
        &connection_headers
    ));
}

#[test]
fn strips_connection_nominated_response_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(CONNECTION, "x-provider-hop".parse().unwrap());
    let connection_headers = ConnectionHeaderNames::from_headers(&headers);

    assert!(!should_forward_response_header(
        &test_header("x-provider-hop"),
        &connection_headers
    ));
}
