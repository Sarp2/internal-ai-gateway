use std::time::Duration;

use axum::http::HeaderMap;

const DEFAULT_CHUNK_COUNT: usize = 2;
const MAX_CHUNK_COUNT: usize = 10_000;
const MAX_CHUNK_DELAY_MS: u64 = 60_000;
const MAX_TOKEN_COUNT: u64 = 1_000_000_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MockResponseType {
    Text,
    ToolUse,
}

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
    pub(crate) chunk_count: usize,
    pub(crate) chunk_delay: Duration,
    pub(crate) http_error: Option<MockHttpError>,
    pub(crate) response_type: MockResponseType,
    pub(crate) stream_error_after_chunks: Option<usize>,
}

impl AnthropicMockControl {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Result<Self, &'static str> {
        let chunk_count = parse_bounded_usize(
            headers,
            "x-mock-chunk-count",
            DEFAULT_CHUNK_COUNT,
            1,
            MAX_CHUNK_COUNT,
            "x-mock-chunk-count must be an integer between 1 and 10000",
        )?;
        let stream_error_after_chunks = parse_optional_bounded_usize(
            headers,
            "x-mock-stream-error-after-chunks",
            0,
            chunk_count,
            "x-mock-stream-error-after-chunks must not exceed x-mock-chunk-count",
        )?;

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
            chunk_count,
            chunk_delay: Duration::from_millis(parse_bounded_u64(
                headers,
                "x-mock-chunk-delay-ms",
                0,
                MAX_CHUNK_DELAY_MS,
                "x-mock-chunk-delay-ms must be a non-negative integer up to 60000",
            )?),
            http_error: parse_http_error(headers)?,
            response_type: parse_response_type(headers)?,
            stream_error_after_chunks,
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

fn parse_response_type(headers: &HeaderMap) -> Result<MockResponseType, &'static str> {
    match header_value(headers, "x-mock-response-type")? {
        None | Some("text") => Ok(MockResponseType::Text),
        Some("tool_use") => Ok(MockResponseType::ToolUse),
        Some(_) => Err("x-mock-response-type must be text or tool_use"),
    }
}

fn parse_bounded_u64(
    headers: &HeaderMap,
    name: &str,
    default: u64,
    maximum: u64,
    error: &'static str,
) -> Result<u64, &'static str> {
    let Some(value) = header_value(headers, name)? else {
        return Ok(default);
    };
    let value = value.parse::<u64>().map_err(|_| error)?;

    (value <= maximum).then_some(value).ok_or(error)
}

fn parse_bounded_usize(
    headers: &HeaderMap,
    name: &str,
    default: usize,
    minimum: usize,
    maximum: usize,
    error: &'static str,
) -> Result<usize, &'static str> {
    let Some(value) = header_value(headers, name)? else {
        return Ok(default);
    };
    let value = value.parse::<usize>().map_err(|_| error)?;

    (minimum..=maximum)
        .contains(&value)
        .then_some(value)
        .ok_or(error)
}

fn parse_optional_bounded_usize(
    headers: &HeaderMap,
    name: &str,
    minimum: usize,
    maximum: usize,
    error: &'static str,
) -> Result<Option<usize>, &'static str> {
    let Some(value) = header_value(headers, name)? else {
        return Ok(None);
    };
    let value = value.parse::<usize>().map_err(|_| error)?;

    (minimum..=maximum)
        .contains(&value)
        .then_some(Some(value))
        .ok_or(error)
}

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, &'static str> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| "mock control headers must contain valid ASCII")
        })
        .transpose()
}
