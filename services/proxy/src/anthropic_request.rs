use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{self, BufReader, Seek, SeekFrom, Write};

use axum::body::Body;
use struson::reader::{JsonReader, JsonStreamReader, ReaderSettings, ValueType};
use tokio::time::Duration;
use tokio_util::io::ReaderStream;

use crate::request_body::timed_request_body_reader;

const IMAGE_INPUT_TOKEN_RESERVE: u64 = 32_768;
const MAX_JSON_NESTING_DEPTH: u32 = 128;
const MAX_SPOOLED_REQUEST_BYTES: u64 = 256 * 1024 * 1024;
const REQUEST_UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct PreparedAnthropicRequest {
    body: reqwest::Body,
    streaming: bool,
    token_budget: u64,
}

impl PreparedAnthropicRequest {
    pub(crate) fn into_parts(self) -> (reqwest::Body, bool, u64) {
        (self.body, self.streaming, self.token_budget)
    }
}

pub(crate) async fn prepare_anthropic_request(
    body: Body,
) -> Result<PreparedAnthropicRequest, AnthropicRequestError> {
    prepare_anthropic_request_with_timeout(body, REQUEST_UPLOAD_TIMEOUT).await
}

async fn prepare_anthropic_request_with_timeout(
    body: Body,
    upload_timeout: Duration,
) -> Result<PreparedAnthropicRequest, AnthropicRequestError> {
    let mut reader =
        timed_request_body_reader(body, upload_timeout, "Anthropic request upload timed out");

    let (file, metadata, body_bytes) = tokio::task::spawn_blocking(move || {
        let mut file = tempfile::tempfile().map_err(AnthropicRequestError::SpoolFailed)?;
        let body_bytes = {
            let mut writer = LimitedSpoolWriter::new(&mut file, MAX_SPOOLED_REQUEST_BYTES);
            io::copy(&mut reader, &mut writer).map_err(AnthropicRequestError::SpoolFailed)?;
            writer.flush().map_err(AnthropicRequestError::SpoolFailed)?;
            writer.written
        };

        file.seek(SeekFrom::Start(0))
            .map_err(AnthropicRequestError::SpoolFailed)?;
        let metadata = inspect_request(JsonStreamReader::new_custom(
            BufReader::new(&mut file),
            reader_settings(),
        ))?;
        file.seek(SeekFrom::Start(0))
            .map_err(AnthropicRequestError::SpoolFailed)?;

        Ok::<_, AnthropicRequestError>((file, metadata, body_bytes))
    })
    .await
    .map_err(|_| AnthropicRequestError::WorkerStopped)??;

    let image_tokens = metadata
        .image_inputs
        .checked_mul(IMAGE_INPUT_TOKEN_RESERVE)
        .ok_or(AnthropicRequestError::TokenBudgetOverflow)?;
    let token_budget = body_bytes
        .checked_add(image_tokens)
        .and_then(|tokens| tokens.checked_add(metadata.max_tokens))
        .ok_or(AnthropicRequestError::TokenBudgetOverflow)?;

    Ok(PreparedAnthropicRequest {
        body: reqwest::Body::wrap_stream(ReaderStream::new(tokio::fs::File::from_std(file))),
        streaming: metadata.streaming,
        token_budget,
    })
}

fn reader_settings() -> ReaderSettings {
    ReaderSettings {
        max_nesting_depth: Some(MAX_JSON_NESTING_DEPTH),
        ..ReaderSettings::default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AnthropicRequestMetadata {
    image_inputs: u64,
    max_tokens: u64,
    streaming: bool,
}

fn inspect_request(
    mut reader: impl JsonReader,
) -> Result<AnthropicRequestMetadata, AnthropicRequestError> {
    reader
        .begin_object()
        .map_err(AnthropicRequestError::InvalidJson)?;
    let mut max_tokens = None;
    let mut streaming = None;
    let mut image_inputs = 0_u64;

    while reader
        .has_next()
        .map_err(AnthropicRequestError::InvalidJson)?
    {
        let name = reader
            .next_name_owned()
            .map_err(AnthropicRequestError::InvalidJson)?;
        match name.as_str() {
            "max_tokens" => {
                if max_tokens.is_some() {
                    return Err(AnthropicRequestError::DuplicateField("max_tokens"));
                }
                max_tokens = Some(
                    reader
                        .next_number::<u64>()
                        .map_err(AnthropicRequestError::InvalidJson)?
                        .map_err(|_| AnthropicRequestError::InvalidMaxTokens)?,
                );
            }
            "stream" => {
                if streaming.is_some() {
                    return Err(AnthropicRequestError::DuplicateField("stream"));
                }
                streaming = Some(
                    reader
                        .next_bool()
                        .map_err(AnthropicRequestError::InvalidJson)?,
                );
            }
            _ => {
                image_inputs = image_inputs
                    .checked_add(count_image_inputs(&mut reader)?)
                    .ok_or(AnthropicRequestError::TokenBudgetOverflow)?;
            }
        }
    }

    reader
        .end_object()
        .map_err(AnthropicRequestError::InvalidJson)?;
    reader
        .consume_trailing_whitespace()
        .map_err(AnthropicRequestError::InvalidJson)?;

    let max_tokens = max_tokens
        .filter(|value| *value > 0)
        .ok_or(AnthropicRequestError::InvalidMaxTokens)?;

    Ok(AnthropicRequestMetadata {
        image_inputs,
        max_tokens,
        streaming: streaming.unwrap_or(false),
    })
}

fn count_image_inputs(reader: &mut impl JsonReader) -> Result<u64, AnthropicRequestError> {
    match reader.peek().map_err(AnthropicRequestError::InvalidJson)? {
        ValueType::Object => {
            reader
                .begin_object()
                .map_err(AnthropicRequestError::InvalidJson)?;
            let mut image_inputs = 0_u64;
            while reader
                .has_next()
                .map_err(AnthropicRequestError::InvalidJson)?
            {
                let name = reader
                    .next_name_owned()
                    .map_err(AnthropicRequestError::InvalidJson)?;
                if name == "type"
                    && reader.peek().map_err(AnthropicRequestError::InvalidJson)?
                        == ValueType::String
                {
                    if reader
                        .next_string()
                        .map_err(AnthropicRequestError::InvalidJson)?
                        == "image"
                    {
                        image_inputs = image_inputs
                            .checked_add(1)
                            .ok_or(AnthropicRequestError::TokenBudgetOverflow)?;
                    }
                } else {
                    image_inputs = image_inputs
                        .checked_add(count_image_inputs(reader)?)
                        .ok_or(AnthropicRequestError::TokenBudgetOverflow)?;
                }
            }
            reader
                .end_object()
                .map_err(AnthropicRequestError::InvalidJson)?;
            Ok(image_inputs)
        }
        ValueType::Array => {
            reader
                .begin_array()
                .map_err(AnthropicRequestError::InvalidJson)?;
            let mut image_inputs = 0_u64;
            while reader
                .has_next()
                .map_err(AnthropicRequestError::InvalidJson)?
            {
                image_inputs = image_inputs
                    .checked_add(count_image_inputs(reader)?)
                    .ok_or(AnthropicRequestError::TokenBudgetOverflow)?;
            }
            reader
                .end_array()
                .map_err(AnthropicRequestError::InvalidJson)?;
            Ok(image_inputs)
        }
        _ => {
            reader
                .skip_value()
                .map_err(AnthropicRequestError::InvalidJson)?;
            Ok(0)
        }
    }
}

struct LimitedSpoolWriter<W> {
    inner: W,
    limit: u64,
    written: u64,
}

impl<W> LimitedSpoolWriter<W> {
    fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            limit,
            written: 0,
        }
    }
}

impl<W: Write> Write for LimitedSpoolWriter<W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if self.written.saturating_add(input.len() as u64) > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::FileTooLarge,
                "Anthropic request exceeds its spool limit",
            ));
        }
        let written = self.inner.write(input)?;
        self.written = self.written.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[derive(Debug)]
pub(crate) enum AnthropicRequestError {
    DuplicateField(&'static str),
    InvalidJson(struson::reader::ReaderError),
    InvalidMaxTokens,
    SpoolFailed(io::Error),
    TokenBudgetOverflow,
    WorkerStopped,
}

impl AnthropicRequestError {
    pub(crate) fn is_client_error(&self) -> bool {
        matches!(
            self,
            Self::DuplicateField(_)
                | Self::InvalidJson(_)
                | Self::InvalidMaxTokens
                | Self::TokenBudgetOverflow
        )
    }

    pub(crate) fn is_request_too_large(&self) -> bool {
        matches!(self, Self::SpoolFailed(error) if error.kind() == io::ErrorKind::FileTooLarge)
    }

    pub(crate) fn is_upload_timeout(&self) -> bool {
        matches!(self, Self::SpoolFailed(error) if error.kind() == io::ErrorKind::TimedOut)
    }
}

impl Display for AnthropicRequestError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateField(field) => write!(formatter, "duplicate Anthropic {field} field"),
            Self::InvalidJson(error) => {
                write!(formatter, "invalid Anthropic JSON request: {error}")
            }
            Self::InvalidMaxTokens => {
                write!(formatter, "Anthropic max_tokens must be a positive integer")
            }
            Self::SpoolFailed(error) => {
                write!(formatter, "failed to spool Anthropic request: {error}")
            }
            Self::TokenBudgetOverflow => write!(formatter, "Anthropic token budget is too large"),
            Self::WorkerStopped => {
                write!(formatter, "Anthropic request preparation worker stopped")
            }
        }
    }
}

impl Error for AnthropicRequestError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            Self::SpoolFailed(error) => Some(error),
            Self::DuplicateField(_)
            | Self::InvalidMaxTokens
            | Self::TokenBudgetOverflow
            | Self::WorkerStopped => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn inspect_slice(body: &[u8]) -> Result<(bool, u64, u64), AnthropicRequestError> {
    let metadata = inspect_request(JsonStreamReader::new_custom(body, reader_settings()))?;
    Ok((
        metadata.streaming,
        metadata.max_tokens,
        metadata.image_inputs,
    ))
}

#[cfg(test)]
pub(crate) async fn prepare_with_upload_timeout(
    body: Body,
    upload_timeout: Duration,
) -> Result<(), AnthropicRequestError> {
    prepare_anthropic_request_with_timeout(body, upload_timeout)
        .await
        .map(|_| ())
}
