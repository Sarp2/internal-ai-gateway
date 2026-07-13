use std::error::Error;
use std::fmt::{Display, Formatter};
use std::io::{self, BufWriter, Read, Write};

use axum::body::{Body, Bytes};
use futures_util::{StreamExt, stream};
use serde_json::{Map, Value};
use struson::reader::{JsonReader, JsonStreamReader, TransferError};
use struson::writer::{JsonStreamWriter, JsonWriter};
use tokio::sync::{mpsc, oneshot};

const BODY_CHANNEL_CAPACITY: usize = 4;
const BODY_CHUNK_BYTES: usize = 16 * 1024;
const MAX_JSON_KEY_BYTES: usize = 8 * 1024;
const MAX_JSON_NESTING_DEPTH: usize = 128;
const MAX_STREAM_CONTROL_BYTES: usize = 64 * 1024;

type InputChunk = Result<Bytes, String>;
type OutputChunk = Result<Bytes, io::Error>;

pub(crate) struct TransformedOpenAiRequest {
    body: reqwest::Body,
    completion: oneshot::Receiver<Result<bool, OpenAiRequestTransformError>>,
}

impl TransformedOpenAiRequest {
    pub fn into_parts(self) -> (reqwest::Body, OpenAiRequestTransformCompletion) {
        (self.body, OpenAiRequestTransformCompletion(self.completion))
    }
}

pub(crate) struct OpenAiRequestTransformCompletion(
    oneshot::Receiver<Result<bool, OpenAiRequestTransformError>>,
);

impl OpenAiRequestTransformCompletion {
    pub async fn finish(self) -> Result<bool, OpenAiRequestTransformError> {
        self.0
            .await
            .map_err(|_| OpenAiRequestTransformError::WorkerStopped)?
    }
}

pub(crate) fn transform_openai_request(body: Body) -> TransformedOpenAiRequest {
    let (input_sender, input_receiver) = mpsc::channel(BODY_CHANNEL_CAPACITY);
    let (output_sender, output_receiver) = mpsc::channel(BODY_CHANNEL_CAPACITY);
    let (completion_sender, completion_receiver) = oneshot::channel();

    tokio::spawn(pump_request_body(body, input_sender));
    tokio::task::spawn_blocking(move || {
        let reader = JsonKeyLimitReader::new(ChannelReader::new(input_receiver));
        let writer = BufWriter::with_capacity(BODY_CHUNK_BYTES, ChannelWriter::new(output_sender));
        let result = transform_json(reader, writer);
        let _ = completion_sender.send(result);
    });

    let output_stream = stream::unfold(output_receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    });

    TransformedOpenAiRequest {
        body: reqwest::Body::wrap_stream(output_stream),
        completion: completion_receiver,
    }
}

async fn pump_request_body(body: Body, sender: mpsc::Sender<InputChunk>) {
    let mut stream = body.into_data_stream();

    while let Some(result) = stream.next().await {
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
                let _ = sender.send(Err(error.to_string())).await;
                return;
            }
        }
    }
}

fn transform_json(
    reader: impl Read,
    writer: impl Write,
) -> Result<bool, OpenAiRequestTransformError> {
    let mut json_reader = JsonStreamReader::new(reader);
    let mut json_writer = JsonStreamWriter::new(writer);
    let mut stream_value = None;
    let mut stream_options = None;

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

    json_writer
        .end_object()
        .map_err(OpenAiRequestTransformError::OutputFailed)?;
    json_writer
        .finish_document()
        .map_err(OpenAiRequestTransformError::OutputFailed)?;

    Ok(streaming)
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
                Some(Err(error)) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
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

struct ChannelWriter {
    sender: mpsc::Sender<OutputChunk>,
}

impl ChannelWriter {
    fn new(sender: mpsc::Sender<OutputChunk>) -> Self {
        Self { sender }
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.is_empty() {
            return Ok(0);
        }

        self.sender
            .blocking_send(Ok(Bytes::copy_from_slice(input)))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "provider body was dropped"))?;
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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
                if self.in_key {
                    self.key_bytes += 1;
                    if self.key_bytes > MAX_JSON_KEY_BYTES {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "JSON object key exceeds its size limit",
                        ));
                    }
                }

                if self.escaped {
                    self.escaped = false;
                } else if *byte == b'\\' {
                    self.escaped = true;
                } else if *byte == b'"' {
                    self.in_string = false;
                    self.in_key = false;
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
    OutputFailed(io::Error),
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
        )
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
            Self::OutputFailed(error) => {
                write!(
                    formatter,
                    "failed to stream transformed OpenAI request: {error}"
                )
            }
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
            | Self::WorkerStopped => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn transform_slice(body: &[u8]) -> Result<(Vec<u8>, bool), OpenAiRequestTransformError> {
    let mut output = Vec::new();
    let streaming = transform_reader(body, &mut output)?;
    Ok((output, streaming))
}

#[cfg(test)]
pub(crate) fn transform_reader(
    reader: impl Read,
    writer: impl Write,
) -> Result<bool, OpenAiRequestTransformError> {
    transform_json(JsonKeyLimitReader::new(reader), writer)
}
