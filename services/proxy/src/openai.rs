use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::State;
use axum::http::header::{ACCEPT_ENCODING, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderName, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures_util::stream;
use futures_util::{Stream, StreamExt};
use reqwest::Client as HttpClient;
use reqwest::redirect::Policy;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::anthropic::{
    ConnectionHeaderNames, should_forward_request_header, should_forward_response_header,
};
use crate::app::AppState;
use crate::auth::AuthError;
use crate::background_tasks::BackgroundTasks;
use crate::engineer_auth::AuthenticatedEngineer;
use crate::rate_limit::RateLimitError;
use crate::streams::OwnedActiveStreamGuard;
use crate::token_usage::{TokenUsageChecker, TokenUsageError};

const OPENAI_CHAT_COMPLETIONS_URL: &str = "https://api.openai.com/v1/chat/completions";
const MAX_REQUEST_BODY_BYTES: usize = 20 * 1024 * 1024;
const STREAM_CHANNEL_CAPACITY: usize = 8;

type ProxyStreamItem = Result<Bytes, Box<dyn Error + Send + Sync>>;

#[derive(Clone)]
pub struct OpenAiProxy {
    api_key: Arc<str>,
    http_client: HttpClient,
}

impl OpenAiProxy {
    pub fn new(api_key: impl Into<Arc<str>>) -> Self {
        Self {
            api_key: api_key.into(),
            http_client: HttpClient::builder()
                .connect_timeout(Duration::from_secs(10))
                .pool_idle_timeout(Duration::from_secs(90))
                .read_timeout(Duration::from_secs(300))
                .redirect(Policy::none())
                .build()
                .expect("OpenAI HTTP client configuration should be valid"),
        }
    }

    async fn forward_chat_completions(
        &self,
        request: Request<Body>,
        engineer: AuthenticatedEngineer,
        token_usage_checker: Arc<TokenUsageChecker>,
        stream_guard: OwnedActiveStreamGuard,
        background_tasks: BackgroundTasks,
    ) -> Result<Response<Body>, OpenAiProxyError> {
        let (parts, body) = request.into_parts();
        let body = to_bytes(body, MAX_REQUEST_BODY_BYTES)
            .await
            .map_err(OpenAiProxyError::RequestBodyReadFailed)?;

        let (provider_body, streaming_request) = prepare_request_body(&body)?;
        let request_connection_headers = ConnectionHeaderNames::from_headers(&parts.headers);
        let mut provider_request = self
            .http_client
            .post(OPENAI_CHAT_COMPLETIONS_URL)
            .bearer_auth(self.api_key.as_ref());

        for (name, value) in &parts.headers {
            if should_forward_openai_request_header(name, &request_connection_headers) {
                provider_request = provider_request.header(name, value);
            }
        }

        let provider_response = provider_request
            .body(provider_body)
            .send()
            .await
            .map_err(OpenAiProxyError::ProviderRequestFailed)?;

        let is_streaming_response =
            streaming_request && is_event_stream_response(provider_response.headers());

        let mut response_builder = Response::builder().status(
            StatusCode::from_u16(provider_response.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY),
        );

        copy_response_headers(provider_response.headers(), response_builder.headers_mut());

        if !is_streaming_response {
            let body = provider_response
                .bytes()
                .await
                .map_err(OpenAiProxyError::ProviderRequestFailed)?;

            if let Err(error) =
                record_usage_from_json_body(&token_usage_checker, &engineer, &body).await
            {
                warn!(%error, "failed to record non-streaming OpenAI token usage");
            }

            return response_builder
                .body(Body::from(body))
                .map_err(OpenAiProxyError::ResponseBuildFailed);
        }

        let stream = usage_recording_stream(
            provider_response.bytes_stream(),
            token_usage_checker,
            engineer,
            stream_guard,
            background_tasks,
        );

        response_builder
            .body(Body::from_stream(stream))
            .map_err(OpenAiProxyError::ResponseBuildFailed)
    }
}

pub async fn chat_completions(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Response<Body> {
    match handle_chat_completions(state, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn handle_chat_completions(
    state: AppState,
    request: Request<Body>,
) -> Result<Response<Body>, OpenAiRouteError> {
    let engineer = state
        .authenticator
        .authenticate_headers(request.headers())
        .await
        .map_err(OpenAiRouteError::Auth)?;

    state
        .rate_limiter
        .check_and_record(&engineer.user_id)
        .await
        .map_err(OpenAiRouteError::RateLimit)?;

    state
        .token_usage_checker
        .check_limits(&engineer)
        .await
        .map_err(OpenAiRouteError::TokenUsage)?;

    let stream_guard = state
        .stream_tracker
        .try_start_owned()
        .ok_or(OpenAiRouteError::TooManyActiveStreams)?;

    state
        .openai_proxy
        .forward_chat_completions(
            request,
            engineer,
            Arc::clone(&state.token_usage_checker),
            stream_guard,
            state.background_tasks.clone(),
        )
        .await
        .map_err(OpenAiRouteError::Proxy)
}

pub(crate) fn prepare_request_body(body: &[u8]) -> Result<(Vec<u8>, bool), OpenAiProxyError> {
    let mut value =
        serde_json::from_slice::<Value>(body).map_err(OpenAiProxyError::InvalidRequestBody)?;
    let streaming = value.get("stream").and_then(Value::as_bool) == Some(true);

    if !streaming {
        return Ok((body.to_vec(), false));
    }

    let object = value
        .as_object_mut()
        .ok_or(OpenAiProxyError::InvalidRequestObject)?;
    let stream_options = object
        .entry("stream_options")
        .or_insert_with(|| Value::Object(Map::new()));

    if stream_options.is_null() {
        *stream_options = Value::Object(Map::new());
    }

    stream_options
        .as_object_mut()
        .ok_or(OpenAiProxyError::InvalidStreamOptions)?
        .insert("include_usage".to_string(), Value::Bool(true));

    serde_json::to_vec(&value)
        .map(|body| (body, true))
        .map_err(OpenAiProxyError::RequestBodySerializationFailed)
}

fn should_forward_openai_request_header(
    name: &HeaderName,
    connection_headers: &ConnectionHeaderNames,
) -> bool {
    should_forward_request_header(name, connection_headers)
        && name != AUTHORIZATION
        && name != ACCEPT_ENCODING
}

fn is_event_stream_response(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"))
}

fn copy_response_headers(provider_headers: &HeaderMap, response_headers: Option<&mut HeaderMap>) {
    let Some(response_headers) = response_headers else {
        return;
    };
    let connection_headers = ConnectionHeaderNames::from_headers(provider_headers);

    for (name, value) in provider_headers {
        if should_forward_response_header(name, &connection_headers) {
            response_headers.append(name, value.clone());
        }
    }
}

pub(crate) fn openai_usage_from_json_slice(body: &[u8]) -> Option<OpenAiUsage> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    openai_usage_from_json_value(value.get("usage")?)
}

fn openai_usage_from_json_value(value: &Value) -> Option<OpenAiUsage> {
    let prompt_tokens = value.get("prompt_tokens").and_then(Value::as_u64);
    let completion_tokens = value.get("completion_tokens").and_then(Value::as_u64);
    let total_tokens = value
        .get("total_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            prompt_tokens
                .zip(completion_tokens)
                .and_then(|(prompt, completion)| prompt.checked_add(completion))
        })?;

    Some(OpenAiUsage {
        completion_tokens,
        prompt_tokens,
        total_tokens,
    })
}

async fn record_usage_from_json_body(
    token_usage_checker: &TokenUsageChecker,
    engineer: &AuthenticatedEngineer,
    body: &Bytes,
) -> Result<(), OpenAiProxyError> {
    let Some(usage) = openai_usage_from_json_slice(body) else {
        return Ok(());
    };

    record_openai_usage(token_usage_checker, engineer, usage).await
}

async fn record_openai_usage(
    token_usage_checker: &TokenUsageChecker,
    engineer: &AuthenticatedEngineer,
    usage: OpenAiUsage,
) -> Result<(), OpenAiProxyError> {
    if usage.total_tokens == 0 {
        return Ok(());
    }

    token_usage_checker
        .record_tokens(engineer, usage.total_tokens)
        .await
        .map_err(OpenAiProxyError::TokenUsageRecordFailed)
}

fn usage_recording_stream(
    provider_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
    token_usage_checker: Arc<TokenUsageChecker>,
    engineer: AuthenticatedEngineer,
    stream_guard: OwnedActiveStreamGuard,
    background_tasks: BackgroundTasks,
) -> impl Stream<Item = ProxyStreamItem> {
    let (sender, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
    let engineer_id = engineer.user_id.clone();

    background_tasks.spawn(async move {
        let mut provider_stream = provider_stream;
        let mut usage_recorder = OpenAiStreamUsageRecorder::new(token_usage_checker, engineer);
        let mut downstream_connected = true;
        let _stream_guard = stream_guard;

        while let Some(provider_result) = provider_stream.next().await {
            match provider_result {
                Ok(chunk) => {
                    usage_recorder.observe_chunk(&chunk);

                    if downstream_connected && sender.send(Ok(chunk)).await.is_err() {
                        downstream_connected = false;
                        warn!(%engineer_id, "OpenAI client disconnected; continuing provider drain");
                    }
                }
                Err(error) => {
                    warn!(%engineer_id, %error, "OpenAI provider stream failed");

                    if downstream_connected {
                        let _ = sender
                            .send(Err(Box::new(error) as Box<dyn Error + Send + Sync>))
                            .await;
                    }

                    break;
                }
            }
        }

        if let Err(error) = usage_recorder.record_observed_usage().await {
            warn!(%engineer_id, %error, "failed to record OpenAI streaming token usage");
        }
    });

    stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    })
}

struct OpenAiStreamUsageRecorder {
    buffered_event: Vec<u8>,
    engineer: AuthenticatedEngineer,
    recording_attempted: bool,
    token_usage_checker: Arc<TokenUsageChecker>,
    usage: Option<OpenAiUsage>,
}

impl OpenAiStreamUsageRecorder {
    fn new(token_usage_checker: Arc<TokenUsageChecker>, engineer: AuthenticatedEngineer) -> Self {
        Self {
            buffered_event: Vec::new(),
            engineer,
            recording_attempted: false,
            token_usage_checker,
            usage: None,
        }
    }

    fn observe_chunk(&mut self, chunk: &[u8]) {
        self.buffered_event.extend_from_slice(chunk);

        while let Some(event_end) = find_sse_event_end(&self.buffered_event) {
            let event = self.buffered_event.drain(..event_end).collect::<Vec<_>>();
            trim_sse_event_separator(&mut self.buffered_event);

            if let Some(usage) = usage_from_sse_event(&event) {
                self.usage = Some(usage);
            }
        }
    }

    async fn record_observed_usage(&mut self) -> Result<(), OpenAiProxyError> {
        self.recording_attempted = true;

        let Some(usage) = self.usage else {
            warn!("OpenAI stream ended without a final usage chunk");
            return Ok(());
        };

        record_openai_usage(&self.token_usage_checker, &self.engineer, usage).await
    }
}

impl Drop for OpenAiStreamUsageRecorder {
    fn drop(&mut self) {
        if self.recording_attempted {
            return;
        }

        let Some(usage) = self.usage else {
            warn!("OpenAI stream was dropped before a final usage chunk was received");
            return;
        };

        let token_usage_checker = Arc::clone(&self.token_usage_checker);
        let engineer = self.engineer.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!("failed to record dropped OpenAI streaming token usage: no Tokio runtime");
            return;
        };

        handle.spawn(async move {
            if let Err(error) = record_openai_usage(&token_usage_checker, &engineer, usage).await {
                warn!(%error, "failed to record dropped OpenAI streaming token usage");
            }
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OpenAiUsage {
    pub completion_tokens: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub total_tokens: u64,
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct OpenAiStreamUsage {
    buffered_event: Vec<u8>,
    usage: Option<OpenAiUsage>,
}

#[cfg(test)]
impl OpenAiStreamUsage {
    pub(crate) fn observe_chunk(&mut self, chunk: &[u8]) {
        self.buffered_event.extend_from_slice(chunk);

        while let Some(event_end) = find_sse_event_end(&self.buffered_event) {
            let event = self.buffered_event.drain(..event_end).collect::<Vec<_>>();
            trim_sse_event_separator(&mut self.buffered_event);

            if let Some(usage) = usage_from_sse_event(&event) {
                self.usage = Some(usage);
            }
        }
    }

    pub(crate) fn observed_usage(&self) -> Option<OpenAiUsage> {
        self.usage
    }
}

fn usage_from_sse_event(event: &[u8]) -> Option<OpenAiUsage> {
    let data = sse_event_data(event)?;

    if data == b"[DONE]" {
        return None;
    }

    let value = serde_json::from_slice::<Value>(&data).ok()?;
    openai_usage_from_json_value(value.get("usage")?)
}

fn find_sse_event_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .or_else(|| buffer.windows(4).position(|window| window == b"\r\n\r\n"))
}

fn trim_sse_event_separator(buffer: &mut Vec<u8>) {
    if buffer.starts_with(b"\r\n\r\n") {
        buffer.drain(..4);
    } else if buffer.starts_with(b"\n\n") {
        buffer.drain(..2);
    }
}

fn sse_event_data(event: &[u8]) -> Option<Vec<u8>> {
    let event_text = String::from_utf8_lossy(event);
    let data_lines = event_text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>();

    (!data_lines.is_empty()).then(|| data_lines.join("\n").into_bytes())
}

pub async fn load_openai_proxy(
    secrets_client: &SecretsManagerClient,
    secret_arn: &str,
) -> Result<OpenAiProxy, OpenAiSecretError> {
    let output = secrets_client
        .get_secret_value()
        .secret_id(secret_arn)
        .send()
        .await
        .map_err(|source| OpenAiSecretError::FetchFailed {
            source: Box::new(source),
        })?;
    let api_key = output
        .secret_string()
        .ok_or(OpenAiSecretError::MissingSecretString)?;

    if api_key.is_empty() {
        return Err(OpenAiSecretError::EmptySecretString);
    }

    Ok(OpenAiProxy::new(api_key.to_string()))
}

#[derive(Debug)]
enum OpenAiRouteError {
    Auth(AuthError),
    RateLimit(RateLimitError),
    TokenUsage(TokenUsageError),
    TooManyActiveStreams,
    Proxy(OpenAiProxyError),
}

impl IntoResponse for OpenAiRouteError {
    fn into_response(self) -> Response<Body> {
        let (status, message) = match &self {
            Self::Auth(AuthError::MissingApiKey)
            | Self::Auth(AuthError::InvalidApiKeyFormat)
            | Self::Auth(AuthError::InvalidCredentials)
            | Self::Auth(AuthError::DisabledEngineer) => {
                (StatusCode::UNAUTHORIZED, self.to_string())
            }
            Self::Auth(AuthError::LookupFailed(_)) => {
                error!(error = %self, "OpenAI auth lookup failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "authentication is temporarily unavailable".to_string(),
                )
            }
            Self::RateLimit(RateLimitError::RateLimitExceeded) => {
                (StatusCode::TOO_MANY_REQUESTS, self.to_string())
            }
            Self::RateLimit(_) => {
                error!(error = %self, "OpenAI rate limit check failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "rate limit check is temporarily unavailable".to_string(),
                )
            }
            Self::TokenUsage(TokenUsageError::DailyLimitExceeded)
            | Self::TokenUsage(TokenUsageError::WeeklyLimitExceeded)
            | Self::TokenUsage(TokenUsageError::LimitExceededDuringInputRecording) => {
                (StatusCode::PAYMENT_REQUIRED, self.to_string())
            }
            Self::TokenUsage(_) => {
                error!(error = %self, "OpenAI token usage check failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "token usage check is temporarily unavailable".to_string(),
                )
            }
            Self::TooManyActiveStreams => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many active streams".to_string(),
            ),
            Self::Proxy(OpenAiProxyError::InvalidRequestBody(_))
            | Self::Proxy(OpenAiProxyError::InvalidRequestObject)
            | Self::Proxy(OpenAiProxyError::InvalidStreamOptions) => {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
            Self::Proxy(_) => {
                error!(error = %self, "OpenAI proxy request failed");
                (StatusCode::BAD_GATEWAY, "OpenAI request failed".to_string())
            }
        };

        (status, axum::Json(json!({ "message": message }))).into_response()
    }
}

impl Display for OpenAiRouteError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auth(error) => write!(formatter, "{error}"),
            Self::RateLimit(error) => write!(formatter, "{error}"),
            Self::TokenUsage(error) => write!(formatter, "{error}"),
            Self::TooManyActiveStreams => write!(formatter, "too many active streams"),
            Self::Proxy(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for OpenAiRouteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Auth(error) => Some(error),
            Self::RateLimit(error) => Some(error),
            Self::TokenUsage(error) => Some(error),
            Self::Proxy(error) => Some(error),
            Self::TooManyActiveStreams => None,
        }
    }
}

#[derive(Debug)]
pub enum OpenAiProxyError {
    InvalidRequestBody(serde_json::Error),
    InvalidRequestObject,
    InvalidStreamOptions,
    ProviderRequestFailed(reqwest::Error),
    RequestBodyReadFailed(axum::Error),
    RequestBodySerializationFailed(serde_json::Error),
    ResponseBuildFailed(axum::http::Error),
    TokenUsageRecordFailed(TokenUsageError),
}

impl Display for OpenAiProxyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequestBody(error) => {
                write!(formatter, "invalid OpenAI JSON request: {error}")
            }
            Self::InvalidRequestObject => {
                write!(formatter, "OpenAI request body must be a JSON object")
            }
            Self::InvalidStreamOptions => {
                write!(formatter, "OpenAI stream_options must be a JSON object")
            }
            Self::ProviderRequestFailed(error) => {
                write!(formatter, "failed to call OpenAI: {error}")
            }
            Self::RequestBodyReadFailed(error) => {
                write!(formatter, "failed to read OpenAI request body: {error}")
            }
            Self::RequestBodySerializationFailed(error) => {
                write!(formatter, "failed to serialize OpenAI request: {error}")
            }
            Self::ResponseBuildFailed(error) => {
                write!(formatter, "failed to build OpenAI response: {error}")
            }
            Self::TokenUsageRecordFailed(error) => {
                write!(formatter, "failed to record OpenAI token usage: {error}")
            }
        }
    }
}

impl Error for OpenAiProxyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidRequestBody(error) => Some(error),
            Self::ProviderRequestFailed(error) => Some(error),
            Self::RequestBodyReadFailed(error) => Some(error),
            Self::RequestBodySerializationFailed(error) => Some(error),
            Self::ResponseBuildFailed(error) => Some(error),
            Self::TokenUsageRecordFailed(error) => Some(error),
            Self::InvalidRequestObject | Self::InvalidStreamOptions => None,
        }
    }
}

#[derive(Debug)]
pub enum OpenAiSecretError {
    FetchFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    MissingSecretString,
    EmptySecretString,
}

impl Display for OpenAiSecretError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FetchFailed { source } => {
                write!(formatter, "failed to fetch OpenAI API key secret: {source}")
            }
            Self::MissingSecretString => write!(
                formatter,
                "OpenAI API key secret must contain a string value"
            ),
            Self::EmptySecretString => write!(formatter, "OpenAI API key secret must not be empty"),
        }
    }
}

impl Error for OpenAiSecretError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FetchFailed { source } => Some(source.as_ref()),
            Self::MissingSecretString | Self::EmptySecretString => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn test_proxy(api_key: &str) -> OpenAiProxy {
    OpenAiProxy::new(api_key.to_string())
}

#[cfg(test)]
pub(crate) fn forwards_request_header(name: &HeaderName, headers: &HeaderMap) -> bool {
    should_forward_openai_request_header(name, &ConnectionHeaderNames::from_headers(headers))
}

#[cfg(test)]
pub(crate) fn request_headers_recomputed_by_client() -> [HeaderName; 2] {
    [axum::http::header::HOST, axum::http::header::CONTENT_LENGTH]
}

#[cfg(test)]
pub(crate) fn test_usage_recording_stream(
    provider_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
    token_usage_checker: Arc<TokenUsageChecker>,
    engineer: AuthenticatedEngineer,
    stream_guard: OwnedActiveStreamGuard,
    background_tasks: BackgroundTasks,
) -> impl Stream<Item = ProxyStreamItem> {
    usage_recording_stream(
        provider_stream,
        token_usage_checker,
        engineer,
        stream_guard,
        background_tasks,
    )
}
