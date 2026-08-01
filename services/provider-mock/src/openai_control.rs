use axum::http::HeaderMap;

use crate::mock_control::{MockControl, header_value, parse_bounded_u64};

const OPENAI_STREAM_EVENT_OVERHEAD: u64 = 4;
const DEFAULT_PROMPT_TOKENS: u64 = 12;
const DEFAULT_COMPLETION_TOKENS: u64 = 6;
const MAX_TOKEN_COUNT: u64 = 1_000_000_000;

#[derive(Clone, Copy)]
pub(crate) enum MockHttpError {
    InvalidRequest,
    Authentication,
    Permission,
    NotFound,
    Conflict,
    RequestTooLarge,
    UnprocessableEntity,
    RateLimit,
    Api,
    Unavailable,
    Timeout,
}

pub(crate) struct OpenAiMockControl {
    pub(crate) cached_prompt_tokens: u64,
    pub(crate) common: MockControl,
    pub(crate) completion_tokens: u64,
    pub(crate) http_error: Option<MockHttpError>,
    pub(crate) prompt_tokens: u64,
}

impl OpenAiMockControl {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Result<Self, &'static str> {
        let prompt_tokens = parse_bounded_u64(
            headers,
            "x-mock-prompt-tokens",
            DEFAULT_PROMPT_TOKENS,
            MAX_TOKEN_COUNT,
            "x-mock-prompt-tokens must be a non-negative integer up to 1000000000",
        )?;
        let cached_prompt_tokens = parse_bounded_u64(
            headers,
            "x-mock-cached-prompt-tokens",
            0,
            prompt_tokens,
            "x-mock-cached-prompt-tokens must not exceed x-mock-prompt-tokens",
        )?;

        Ok(Self {
            cached_prompt_tokens,
            common: MockControl::from_headers(headers, OPENAI_STREAM_EVENT_OVERHEAD)?,
            completion_tokens: parse_bounded_u64(
                headers,
                "x-mock-completion-tokens",
                DEFAULT_COMPLETION_TOKENS,
                MAX_TOKEN_COUNT,
                "x-mock-completion-tokens must be a non-negative integer up to 1000000000",
            )?,
            http_error: parse_http_error(headers)?,
            prompt_tokens,
        })
    }
}

fn parse_http_error(headers: &HeaderMap) -> Result<Option<MockHttpError>, &'static str> {
    match header_value(headers, "x-mock-http-status")? {
        None => Ok(None),
        Some("400") => Ok(Some(MockHttpError::InvalidRequest)),
        Some("401") => Ok(Some(MockHttpError::Authentication)),
        Some("403") => Ok(Some(MockHttpError::Permission)),
        Some("404") => Ok(Some(MockHttpError::NotFound)),
        Some("409") => Ok(Some(MockHttpError::Conflict)),
        Some("413") => Ok(Some(MockHttpError::RequestTooLarge)),
        Some("422") => Ok(Some(MockHttpError::UnprocessableEntity)),
        Some("429") => Ok(Some(MockHttpError::RateLimit)),
        Some("500") => Ok(Some(MockHttpError::Api)),
        Some("503") => Ok(Some(MockHttpError::Unavailable)),
        Some("504") => Ok(Some(MockHttpError::Timeout)),
        Some(_) => Err(
            "x-mock-http-status must be 400, 401, 403, 404, 409, 413, 422, 429, 500, 503, or 504",
        ),
    }
}
