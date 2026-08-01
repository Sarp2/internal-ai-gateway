use axum::http::HeaderMap;

use crate::mock_control::{MockControl, header_value, parse_bounded_u64};

const ANTHROPIC_STREAM_EVENT_OVERHEAD: u64 = 5;
const MAX_TOKEN_COUNT: u64 = 1_000_000_000;

#[derive(Clone, Copy)]
pub(crate) enum MockHttpError {
    InvalidRequest,
    Authentication,
    Billing,
    Permission,
    NotFound,
    Conflict,
    RequestTooLarge,
    RateLimit,
    Api,
    Timeout,
    Overloaded,
}

pub(crate) struct AnthropicMockControl {
    pub(crate) cache_creation_input_tokens: u64,
    pub(crate) cache_read_input_tokens: u64,
    pub(crate) common: MockControl,
    pub(crate) http_error: Option<MockHttpError>,
}

impl AnthropicMockControl {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Result<Self, &'static str> {
        Ok(Self {
            cache_creation_input_tokens: parse_bounded_u64(
                headers,
                "x-mock-cache-creation-input-tokens",
                0,
                MAX_TOKEN_COUNT,
                "x-mock-cache-creation-input-tokens must be a non-negative integer up to 1000000000",
            )?,
            cache_read_input_tokens: parse_bounded_u64(
                headers,
                "x-mock-cache-read-input-tokens",
                0,
                MAX_TOKEN_COUNT,
                "x-mock-cache-read-input-tokens must be a non-negative integer up to 1000000000",
            )?,
            common: MockControl::from_headers(headers, ANTHROPIC_STREAM_EVENT_OVERHEAD)?,
            http_error: parse_http_error(headers)?,
        })
    }
}

fn parse_http_error(headers: &HeaderMap) -> Result<Option<MockHttpError>, &'static str> {
    match header_value(headers, "x-mock-http-status")? {
        None => Ok(None),
        Some("400") => Ok(Some(MockHttpError::InvalidRequest)),
        Some("401") => Ok(Some(MockHttpError::Authentication)),
        Some("402") => Ok(Some(MockHttpError::Billing)),
        Some("403") => Ok(Some(MockHttpError::Permission)),
        Some("404") => Ok(Some(MockHttpError::NotFound)),
        Some("409") => Ok(Some(MockHttpError::Conflict)),
        Some("413") => Ok(Some(MockHttpError::RequestTooLarge)),
        Some("429") => Ok(Some(MockHttpError::RateLimit)),
        Some("500") => Ok(Some(MockHttpError::Api)),
        Some("504") => Ok(Some(MockHttpError::Timeout)),
        Some("529") => Ok(Some(MockHttpError::Overloaded)),
        Some(_) => Err(
            "x-mock-http-status must be 400, 401, 402, 403, 404, 409, 413, 429, 500, 504, or 529",
        ),
    }
}
