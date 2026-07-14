use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};

use axum::body::{Body, Bytes};
use futures_util::StreamExt;
use serde_json::{Map, Value};
use struson::reader::{JsonReader, JsonStreamReader, TransferError, ValueType};
use struson::writer::{JsonStreamWriter, JsonWriter};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant, timeout_at};
use tokio_util::io::ReaderStream;

const BODY_CHANNEL_CAPACITY: usize = 4;
const BODY_CHUNK_BYTES: usize = 16 * 1024;
const IMAGE_INPUT_TOKEN_RESERVE: u64 = 32_768;
const MAX_JSON_KEY_BYTES: usize = 8 * 1024;
const MAX_JSON_NESTING_DEPTH: usize = 128;
const MAX_STREAM_CONTROL_BYTES: usize = 64 * 1024;
const MAX_SPOOLED_REQUEST_BYTES: u64 = 256 * 1024 * 1024;
const REQUEST_UPLOAD_TIMEOUT: Duration = Duration::from_secs(60);

type InputChunk = io::Result<Bytes>;

pub(crate) struct PreparedOpenAiRequest {
    body: reqwest::Body,
    streaming: bool,
    token_budget: u64,
}

impl PreparedOpenAiRequest {
    pub fn into_parts(self) -> (reqwest::Body, bool, u64) {
        (self.body, self.streaming, self.token_budget)
    }
}

pub(crate) async fn prepare_openai_request(
    body: Body,
    default_max_completion_tokens: u64,
) -> Result<PreparedOpenAiRequest, OpenAiRequestTransformError> {
    prepare_openai_request_with_timeout(body, default_max_completion_tokens, REQUEST_UPLOAD_TIMEOUT)
        .await
}

async fn prepare_openai_request_with_timeout(
    body: Body,
    default_max_completion_tokens: u64,
    upload_timeout: Duration,
) -> Result<PreparedOpenAiRequest, OpenAiRequestTransformError> {
    let (input_sender, input_receiver) = mpsc::channel(BODY_CHANNEL_CAPACITY);

    tokio::spawn(pump_request_body(body, input_sender, upload_timeout));
    let (file, metadata, body_bytes, image_inputs) = tokio::task::spawn_blocking(move || {
        let mut file = tempfile::tempfile().map_err(OpenAiRequestTransformError::OutputFailed)?;
        let reader = JsonKeyLimitReader::new(ChannelReader::new(input_receiver));
        let writer = BufWriter::with_capacity(
            BODY_CHUNK_BYTES,
            LimitedSpoolWriter::new(&mut file, MAX_SPOOLED_REQUEST_BYTES),
        );

        let metadata = transform_json(reader, writer, default_max_completion_tokens)?;
        let body_bytes = file
            .stream_position()
            .map_err(OpenAiRequestTransformError::OutputFailed)?;
        file.seek(SeekFrom::Start(0))
            .map_err(OpenAiRequestTransformError::OutputFailed)?;

        let image_inputs =
            count_image_inputs(&mut JsonStreamReader::new(BufReader::new(&mut file)))?;
        file.seek(SeekFrom::Start(0))
            .map_err(OpenAiRequestTransformError::OutputFailed)?;
        Ok::<_, OpenAiRequestTransformError>((file, metadata, body_bytes, image_inputs))
    })
    .await
    .map_err(|_| OpenAiRequestTransformError::WorkerStopped)??;

    let token_budget =
        calculate_token_budget(body_bytes, metadata.max_output_tokens, image_inputs)?;
    let file = tokio::fs::File::from_std(file);

    Ok(PreparedOpenAiRequest {
        body: reqwest::Body::wrap_stream(ReaderStream::new(file)),
        streaming: metadata.streaming,
        token_budget,
    })
}

fn count_image_inputs(
    json_reader: &mut impl JsonReader,
) -> Result<u64, OpenAiRequestTransformError> {
    match json_reader
        .peek()
        .map_err(OpenAiRequestTransformError::InvalidJson)?
    {
        ValueType::Object => {
            json_reader
                .begin_object()
                .map_err(OpenAiRequestTransformError::InvalidJson)?;
            let mut image_inputs = 0_u64;

            while json_reader
                .has_next()
                .map_err(OpenAiRequestTransformError::InvalidJson)?
            {
                let name = json_reader
                    .next_name_owned()
                    .map_err(OpenAiRequestTransformError::InvalidJson)?;
                if name == "image_url" {
                    image_inputs = image_inputs
                        .checked_add(1)
                        .ok_or(OpenAiRequestTransformError::TokenBudgetOverflow)?;
                }
                image_inputs = image_inputs
                    .checked_add(count_image_inputs(json_reader)?)
                    .ok_or(OpenAiRequestTransformError::TokenBudgetOverflow)?;
            }

            json_reader
                .end_object()
                .map_err(OpenAiRequestTransformError::InvalidJson)?;
            Ok(image_inputs)
        }
        ValueType::Array => {
            json_reader
                .begin_array()
                .map_err(OpenAiRequestTransformError::InvalidJson)?;
            let mut image_inputs = 0_u64;

            while json_reader
                .has_next()
                .map_err(OpenAiRequestTransformError::InvalidJson)?
            {
                image_inputs = image_inputs
                    .checked_add(count_image_inputs(json_reader)?)
                    .ok_or(OpenAiRequestTransformError::TokenBudgetOverflow)?;
            }

            json_reader
                .end_array()
                .map_err(OpenAiRequestTransformError::InvalidJson)?;
            Ok(image_inputs)
        }
        _ => {
            json_reader
                .skip_value()
                .map_err(OpenAiRequestTransformError::InvalidJson)?;
            Ok(0)
        }
    }
}

fn calculate_token_budget(
    body_bytes: u64,
    max_output_tokens: u64,
    image_inputs: u64,
) -> Result<u64, OpenAiRequestTransformError> {
    let image_token_reserve = image_inputs
        .checked_mul(IMAGE_INPUT_TOKEN_RESERVE)
        .ok_or(OpenAiRequestTransformError::TokenBudgetOverflow)?;

    body_bytes
        .checked_add(image_token_reserve)
        .and_then(|input_budget| input_budget.checked_add(max_output_tokens))
        .ok_or(OpenAiRequestTransformError::TokenBudgetOverflow)
}

async fn pump_request_body(body: Body, sender: mpsc::Sender<InputChunk>, upload_timeout: Duration) {
    let mut stream = body.into_data_stream();
    let deadline = Instant::now() + upload_timeout;

    loop {
        let result = match timeout_at(deadline, stream.next()).await {
            Ok(Some(result)) => result,
            Ok(None) => return,
            Err(_) => {
                let _ = sender
                    .send(Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "OpenAI request upload timed out",
                    )))
                    .await;
                return;
            }
        };

        match result {
            Ok(chunk) => {
                for section in chunk.chunks(BODY_CHUNK_BYTES) {
                    if sender
                        .send(Ok(Bytes::copy_from_slice(section)))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(error) => {
                let _ = sender
                    .send(Err(io::Error::new(io::ErrorKind::InvalidData, error)))
                    .await;
                return;
            }
        }
    }
}

fn transform_json(
    reader: impl Read,
    writer: impl Write,
    default_max_completion_tokens: u64,
) -> Result<OpenAiRequestMetadata, OpenAiRequestTransformError> {
    let mut json_reader = JsonStreamReader::new(reader);
    let mut json_writer = JsonStreamWriter::new(writer);
    let mut stream_value = None;
    let mut stream_options = None;
    let mut max_completion_tokens = None;
    let mut max_tokens = None;

    json_reader
        .begin_object()
        .map_err(OpenAiRequestTransformError::InvalidJson)?;
    json_writer
        .begin_object()
        .map_err(OpenAiRequestTransformError::OutputFailed)?;

    while json_reader
        .has_next()
        .map_err(OpenAiRequestTransformError::InvalidJson)?
    {
        let name = json_reader
            .next_name_owned()
            .map_err(OpenAiRequestTransformError::InvalidJson)?;

        match name.as_str() {
            "stream" => {
                if stream_value.is_some() {
                    return Err(OpenAiRequestTransformError::DuplicateControlField("stream"));
                }
                stream_value = Some(capture_control_value(&mut json_reader, "stream")?);
            }
            "stream_options" => {
                if stream_options.is_some() {
                    return Err(OpenAiRequestTransformError::DuplicateControlField(
                        "stream_options",
                    ));
                }
                stream_options = Some(capture_control_value(&mut json_reader, "stream_options")?);
            }
            "max_completion_tokens" => {
                if max_completion_tokens.is_some() {
                    return Err(OpenAiRequestTransformError::DuplicateControlField(
                        "max_completion_tokens",
                    ));
                }
                max_completion_tokens = Some(capture_control_value(
                    &mut json_reader,
                    "max_completion_tokens",
                )?);
            }
            "max_tokens" => {
                if max_tokens.is_some() {
                    return Err(OpenAiRequestTransformError::DuplicateControlField(
                        "max_tokens",
                    ));
                }
                max_tokens = Some(capture_control_value(&mut json_reader, "max_tokens")?);
            }
            _ => {
                json_writer
                    .name(&name)
                    .map_err(OpenAiRequestTransformError::OutputFailed)?;
                json_reader
                    .transfer_to(&mut json_writer)
                    .map_err(map_transfer_error)?;
            }
        }
    }

    json_reader
        .end_object()
        .map_err(OpenAiRequestTransformError::InvalidJson)?;
    json_reader
        .consume_trailing_whitespace()
        .map_err(OpenAiRequestTransformError::InvalidJson)?;

    let streaming = stream_value
        .as_deref()
        .and_then(|value| serde_json::from_slice::<Value>(value).ok())
        .and_then(|value| value.as_bool())
        == Some(true);

    if let Some(stream_value) = stream_value {
        write_captured_member(&mut json_writer, "stream", &stream_value)?;
    }

    match (streaming, stream_options) {
        (true, stream_options) => {
            let stream_options = force_usage_option(stream_options.as_deref())?;
            write_captured_member(&mut json_writer, "stream_options", &stream_options)?;
        }
        (false, Some(stream_options)) => {
            write_captured_member(&mut json_writer, "stream_options", &stream_options)?;
        }
        (false, None) => {}
    }

    let max_output_tokens = match (&max_completion_tokens, &max_tokens) {
        (Some(value), _) => parse_positive_token_limit(value, "max_completion_tokens")?,
        (None, Some(value)) => parse_positive_token_limit(value, "max_tokens")?,
        (None, None) => default_max_completion_tokens,
    };

    if let Some(value) = max_completion_tokens {
        write_captured_member(&mut json_writer, "max_completion_tokens", &value)?;
    } else if max_tokens.is_none() {
        json_writer
            .name("max_completion_tokens")
            .map_err(OpenAiRequestTransformError::OutputFailed)?;
        json_writer
            .number_value(max_output_tokens)
            .map_err(OpenAiRequestTransformError::OutputFailed)?;
    }

    if let Some(value) = max_tokens {
        write_captured_member(&mut json_writer, "max_tokens", &value)?;
    }

    json_writer
        .end_object()
        .map_err(OpenAiRequestTransformError::OutputFailed)?;
    let mut writer = json_writer
        .finish_document()
        .map_err(OpenAiRequestTransformError::OutputFailed)?;
    writer
        .flush()
        .map_err(OpenAiRequestTransformError::OutputFailed)?;

    Ok(OpenAiRequestMetadata {
        max_output_tokens,
        streaming,
    })
}

fn parse_positive_token_limit(
    value: &[u8],
    field: &'static str,
) -> Result<u64, OpenAiRequestTransformError> {
    serde_json::from_slice::<Value>(value)
        .ok()
        .and_then(|value| value.as_u64())
        .filter(|value| *value > 0)
        .ok_or(OpenAiRequestTransformError::InvalidTokenLimit(field))
}

#[derive(Clone, Copy)]
struct OpenAiRequestMetadata {
    max_output_tokens: u64,
    streaming: bool,
}

fn capture_control_value(
    json_reader: &mut impl JsonReader,
    field: &'static str,
) -> Result<Vec<u8>, OpenAiRequestTransformError> {
    let mut output = LimitedBuffer::new(MAX_STREAM_CONTROL_BYTES);
    let transfer_result = {
        let mut json_writer = JsonStreamWriter::new(&mut output);
        let result = json_reader.transfer_to(&mut json_writer);

        match result {
            Ok(()) => json_writer
                .finish_document()
                .map(|_| ())
                .map_err(TransferError::WriterError),
            Err(error) => Err(error),
        }
    };

    if output.limit_exceeded {
        return Err(OpenAiRequestTransformError::ControlFieldTooLarge(field));
    }

    transfer_result.map_err(map_transfer_error)?;
    Ok(output.bytes)
}

fn force_usage_option(
    stream_options: Option<&[u8]>,
) -> Result<Vec<u8>, OpenAiRequestTransformError> {
    let mut value = match stream_options {
        Some(value) => serde_json::from_slice::<Value>(value)
            .map_err(OpenAiRequestTransformError::InvalidControlJson)?,
        None => Value::Object(Map::new()),
    };

    if value.is_null() {
        value = Value::Object(Map::new());
    }

    value
        .as_object_mut()
        .ok_or(OpenAiRequestTransformError::InvalidStreamOptions)?
        .insert("include_usage".to_string(), Value::Bool(true));

    serde_json::to_vec(&value).map_err(OpenAiRequestTransformError::InvalidControlJson)
}

fn write_captured_member(
    json_writer: &mut impl JsonWriter,
    name: &str,
    value: &[u8],
) -> Result<(), OpenAiRequestTransformError> {
    json_writer
        .name(name)
        .map_err(OpenAiRequestTransformError::OutputFailed)?;

    let mut value_reader = JsonStreamReader::new(value);
    value_reader
        .transfer_to(json_writer)
        .map_err(map_transfer_error)?;
    value_reader
        .consume_trailing_whitespace()
        .map_err(OpenAiRequestTransformError::InvalidJson)
}

fn map_transfer_error(error: TransferError) -> OpenAiRequestTransformError {
    match error {
        TransferError::ReaderError(error) => OpenAiRequestTransformError::InvalidJson(error),
        TransferError::WriterError(error) => OpenAiRequestTransformError::OutputFailed(error),
    }
}

struct ChannelReader {
    current: Bytes,
    offset: usize,
    receiver: mpsc::Receiver<InputChunk>,
}

impl ChannelReader {
    fn new(receiver: mpsc::Receiver<InputChunk>) -> Self {
        Self {
            current: Bytes::new(),
            offset: 0,
            receiver,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        while self.offset == self.current.len() {
            match self.receiver.blocking_recv() {
                Some(Ok(chunk)) => {
                    self.current = chunk;
                    self.offset = 0;
                }
                Some(Err(error)) => return Err(error),
                None => return Ok(0),
            }
        }

        let bytes_to_copy = output.len().min(self.current.len() - self.offset);
        output[..bytes_to_copy]
            .copy_from_slice(&self.current[self.offset..self.offset + bytes_to_copy]);
        self.offset += bytes_to_copy;
        Ok(bytes_to_copy)
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
                "OpenAI request exceeds its spool limit",
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

struct LimitedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    limit_exceeded: bool,
}

impl LimitedBuffer {
    fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_exceeded: false,
        }
    }
}

impl Write for LimitedBuffer {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if self.bytes.len().saturating_add(input.len()) > self.limit {
            self.limit_exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "OpenAI control field exceeds its size limit",
            ));
        }

        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ContainerKind {
    Array,
    Object { expecting_key: bool },
}

struct JsonKeyLimitReader<R> {
    inner: R,
    containers: Vec<ContainerKind>,
    escaped: bool,
    in_key: bool,
    in_string: bool,
    key_bytes: usize,
}

impl<R> JsonKeyLimitReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            containers: Vec::new(),
            escaped: false,
            in_key: false,
            in_string: false,
            key_bytes: 0,
        }
    }

    fn inspect(&mut self, bytes: &[u8]) -> io::Result<()> {
        for byte in bytes {
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if *byte == b'\\' {
                    self.escaped = true;
                } else if *byte == b'"' {
                    self.in_string = false;
                    self.in_key = false;
                    continue;
                }

                if self.in_key {
                    self.key_bytes += 1;
                    if self.key_bytes > MAX_JSON_KEY_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "JSON object key exceeds its size limit",
                        ));
                    }
                }
                continue;
            }

            match *byte {
                b'"' => {
                    self.in_string = true;
                    self.in_key = matches!(
                        self.containers.last(),
                        Some(ContainerKind::Object {
                            expecting_key: true
                        })
                    );
                    self.key_bytes = 0;
                }
                b'{' => self.push_container(ContainerKind::Object {
                    expecting_key: true,
                })?,
                b'[' => self.push_container(ContainerKind::Array)?,
                b'}' | b']' => {
                    self.containers.pop();
                }
                b':' => {
                    if let Some(ContainerKind::Object { expecting_key }) =
                        self.containers.last_mut()
                    {
                        *expecting_key = false;
                    }
                }
                b',' => {
                    if let Some(ContainerKind::Object { expecting_key }) =
                        self.containers.last_mut()
                    {
                        *expecting_key = true;
                    }
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn push_container(&mut self, container: ContainerKind) -> io::Result<()> {
        if self.containers.len() >= MAX_JSON_NESTING_DEPTH {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSON nesting depth exceeds its limit",
            ));
        }
        self.containers.push(container);
        Ok(())
    }
}

impl<R: Read> Read for JsonKeyLimitReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(output)?;
        self.inspect(&output[..read])?;
        Ok(read)
    }
}

#[derive(Debug)]
pub(crate) enum OpenAiRequestTransformError {
    ControlFieldTooLarge(&'static str),
    DuplicateControlField(&'static str),
    InvalidControlJson(serde_json::Error),
    InvalidJson(struson::reader::ReaderError),
    InvalidStreamOptions,
    InvalidTokenLimit(&'static str),
    OutputFailed(io::Error),
    TokenBudgetOverflow,
    WorkerStopped,
}

impl OpenAiRequestTransformError {
    pub fn is_client_error(&self) -> bool {
        matches!(
            self,
            Self::ControlFieldTooLarge(_)
                | Self::DuplicateControlField(_)
                | Self::InvalidControlJson(_)
                | Self::InvalidJson(_)
                | Self::InvalidStreamOptions
                | Self::InvalidTokenLimit(_)
                | Self::TokenBudgetOverflow
        )
    }

    pub fn is_request_too_large(&self) -> bool {
        matches!(self, Self::OutputFailed(error) if error.kind() == io::ErrorKind::FileTooLarge)
    }

    pub fn is_upload_timeout(&self) -> bool {
        match self {
            Self::InvalidJson(struson::reader::ReaderError::IoError { error, .. })
            | Self::OutputFailed(error) => error.kind() == io::ErrorKind::TimedOut,
            _ => false,
        }
    }
}

impl Display for OpenAiRequestTransformError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ControlFieldTooLarge(field) => {
                write!(formatter, "OpenAI {field} exceeds its size limit")
            }
            Self::DuplicateControlField(field) => {
                write!(
                    formatter,
                    "OpenAI request contains duplicate {field} fields"
                )
            }
            Self::InvalidControlJson(error) => write!(formatter, "invalid OpenAI control: {error}"),
            Self::InvalidJson(error) => write!(formatter, "invalid OpenAI JSON request: {error}"),
            Self::InvalidStreamOptions => {
                write!(formatter, "OpenAI stream_options must be a JSON object")
            }
            Self::InvalidTokenLimit(field) => {
                write!(formatter, "OpenAI {field} must be a positive integer")
            }
            Self::OutputFailed(error) => {
                write!(
                    formatter,
                    "failed to stream transformed OpenAI request: {error}"
                )
            }
            Self::TokenBudgetOverflow => write!(formatter, "OpenAI token budget is too large"),
            Self::WorkerStopped => write!(formatter, "OpenAI request transformer stopped"),
        }
    }
}

impl Error for OpenAiRequestTransformError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidControlJson(error) => Some(error),
            Self::InvalidJson(error) => Some(error),
            Self::OutputFailed(error) => Some(error),
            Self::ControlFieldTooLarge(_)
            | Self::DuplicateControlField(_)
            | Self::InvalidStreamOptions
            | Self::InvalidTokenLimit(_)
            | Self::TokenBudgetOverflow
            | Self::WorkerStopped => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn transform_slice(
    body: &[u8],
) -> Result<(Vec<u8>, bool, u64), OpenAiRequestTransformError> {
    let mut output = Vec::new();
    let metadata = transform_json(JsonKeyLimitReader::new(body), &mut output, 32_768)?;
    Ok((output, metadata.streaming, metadata.max_output_tokens))
}

#[cfg(test)]
pub(crate) fn transform_reader(
    reader: impl Read,
    writer: impl Write,
) -> Result<bool, OpenAiRequestTransformError> {
    transform_json(JsonKeyLimitReader::new(reader), writer, 32_768)
        .map(|metadata| metadata.streaming)
}

#[cfg(test)]
pub(crate) async fn prepare_with_upload_timeout(
    body: Body,
    upload_timeout: Duration,
) -> Result<(), OpenAiRequestTransformError> {
    prepare_openai_request_with_timeout(body, 32_768, upload_timeout)
        .await
        .map(|_| ())
}
