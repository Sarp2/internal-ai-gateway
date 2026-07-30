use std::time::Duration;

use axum::http::HeaderMap;

const DEFAULT_CHUNK_COUNT: usize = 2;
const MAX_CHUNK_COUNT: usize = 10_000;
const MAX_CHUNK_DELAY_MS: u64 = 60_000;
const MAX_STREAM_DURATION_MS: u64 = 60 * 60 * 1_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MockResponseType {
    Text,
    ToolUse,
}

pub(crate) struct MockControl {
    pub(crate) chunk_count: usize,
    pub(crate) chunk_delay: Duration,
    pub(crate) response_type: MockResponseType,
    pub(crate) stream_error_after_chunks: Option<usize>,
}

impl MockControl {
    pub(crate) fn from_headers(
        headers: &HeaderMap,
        stream_event_overhead: u64,
    ) -> Result<Self, &'static str> {
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
        let chunk_delay_ms = parse_bounded_u64(
            headers,
            "x-mock-chunk-delay-ms",
            0,
            MAX_CHUNK_DELAY_MS,
            "x-mock-chunk-delay-ms must be a non-negative integer up to 60000",
        )?;
        validate_stream_duration(chunk_count, chunk_delay_ms, stream_event_overhead)?;

        Ok(Self {
            chunk_count,
            chunk_delay: Duration::from_millis(chunk_delay_ms),
            response_type: parse_response_type(headers)?,
            stream_error_after_chunks,
        })
    }
}

fn validate_stream_duration(
    chunk_count: usize,
    chunk_delay_ms: u64,
    stream_event_overhead: u64,
) -> Result<(), &'static str> {
    let chunk_count =
        u64::try_from(chunk_count).expect("bounded mock chunk count should fit into u64");
    let event_count = chunk_count
        .checked_add(stream_event_overhead)
        .ok_or("mock stream duration must not exceed one hour")?;
    let duration_ms = event_count
        .checked_mul(chunk_delay_ms)
        .ok_or("mock stream duration must not exceed one hour")?;

    (duration_ms <= MAX_STREAM_DURATION_MS)
        .then_some(())
        .ok_or("mock stream duration must not exceed one hour")
}

fn parse_response_type(headers: &HeaderMap) -> Result<MockResponseType, &'static str> {
    match header_value(headers, "x-mock-response-type")? {
        None | Some("text") => Ok(MockResponseType::Text),
        Some("tool_use") => Ok(MockResponseType::ToolUse),
        Some(_) => Err("x-mock-response-type must be text or tool_use"),
    }
}

pub(crate) fn parse_bounded_u64(
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

pub(crate) fn header_value<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a str>, &'static str> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| "mock control headers must contain valid ASCII")
        })
        .transpose()
}
