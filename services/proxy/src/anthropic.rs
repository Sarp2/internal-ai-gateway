use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use axum::body::Body;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::header::{
    CONNECTION, CONTENT_LENGTH, HOST, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, TE, TRAILER,
    TRANSFER_ENCODING, UPGRADE,
};
use axum::http::{HeaderMap, HeaderName, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures_util::stream;
use futures_util::{Stream, StreamExt};
use reqwest::Client as HttpClient;
use reqwest::redirect::Policy;
use serde_json::{Value, json};
use tracing::{error, warn};

use crate::app::AppState;
use crate::auth::AuthError;
use crate::engineer_auth::AuthenticatedEngineer;
use crate::rate_limit::RateLimitError;
use crate::streams::OwnedActiveStreamGuard;
use crate::token_usage::{TokenUsageChecker, TokenUsageError};

const ANTHROPIC_MESSAGES_URL: &str = "https://api.anthropic.com/v1/messages";
const INTERNAL_API_KEY_HEADER: &str = "x-api-key";

#[derive(Clone)]
pub struct AnthropicProxy {
    api_key: Arc<str>,
    http_client: HttpClient,
}

impl AnthropicProxy {
    pub fn new(api_key: impl Into<Arc<str>>) -> Self {
        Self {
            api_key: api_key.into(),
            http_client: HttpClient::builder()
                .connect_timeout(Duration::from_secs(10))
                .pool_idle_timeout(Duration::from_secs(90))
                .read_timeout(Duration::from_secs(300))
                .redirect(Policy::none())
                .build()
                .expect("Anthropic HTTP client configuration should be valid"),
        }
    }

    async fn forward_messages(
        &self,
        request: Request<Body>,
        engineer: AuthenticatedEngineer,
        token_usage_checker: Arc<TokenUsageChecker>,
        stream_guard: OwnedActiveStreamGuard,
    ) -> Result<Response<Body>, AnthropicProxyError> {
        let (parts, body) = request.into_parts();
        let request_connection_headers = ConnectionHeaderNames::from_headers(&parts.headers);
        let mut provider_request = self
            .http_client
            .post(ANTHROPIC_MESSAGES_URL)
            .header(INTERNAL_API_KEY_HEADER, self.api_key.as_ref());

        for (name, value) in parts.headers.iter() {
            if should_forward_request_header(name, &request_connection_headers) {
                provider_request = provider_request.header(name, value);
            }
        }

        let provider_response = provider_request
            .body(reqwest::Body::wrap_stream(body.into_data_stream()))
            .send()
            .await
            .map_err(AnthropicProxyError::ProviderRequestFailed)?;

        let is_streaming_response = is_event_stream_response(provider_response.headers());
        let mut response_builder = Response::builder().status(
            StatusCode::from_u16(provider_response.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY),
        );

        copy_response_headers(provider_response.headers(), response_builder.headers_mut());

        if !is_streaming_response {
            let body = provider_response
                .bytes()
                .await
                .map_err(AnthropicProxyError::ProviderRequestFailed)?;
            if let Err(error) =
                record_usage_from_json_body(&token_usage_checker, &engineer, &body).await
            {
                warn!(%error, "failed to record non-streaming Anthropic token usage");
            }

            return response_builder
                .body(Body::from(body))
                .map_err(AnthropicProxyError::ResponseBuildFailed);
        }

        let stream = usage_recording_stream(
            provider_response.bytes_stream(),
            token_usage_checker,
            engineer,
            stream_guard,
        );

        response_builder
            .body(Body::from_stream(stream))
            .map_err(AnthropicProxyError::ResponseBuildFailed)
    }
}

pub async fn messages(State(state): State<AppState>, request: Request<Body>) -> Response<Body> {
    match handle_messages(state, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn handle_messages(
    state: AppState,
    request: Request<Body>,
) -> Result<Response<Body>, AnthropicRouteError> {
    let engineer = state
        .authenticator
        .authenticate_headers(request.headers())
        .await
        .map_err(AnthropicRouteError::Auth)?;

    state
        .rate_limiter
        .check_and_record(&engineer.user_id)
        .await
        .map_err(AnthropicRouteError::RateLimit)?;

    state
        .token_usage_checker
        .check_limits(&engineer)
        .await
        .map_err(AnthropicRouteError::TokenUsage)?;

    let stream_guard = state
        .stream_tracker
        .try_start_owned()
        .ok_or(AnthropicRouteError::TooManyActiveStreams)?;

    state
        .anthropic_proxy
        .forward_messages(
            request,
            engineer,
            Arc::clone(&state.token_usage_checker),
            stream_guard,
        )
        .await
        .map_err(AnthropicRouteError::Proxy)
}

fn is_event_stream_response(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"))
}

async fn record_anthropic_usage(
    token_usage_checker: &TokenUsageChecker,
    engineer: &AuthenticatedEngineer,
    usage: AnthropicUsage,
) -> Result<(), AnthropicProxyError> {
    let token_count = usage.total_tokens();

    if token_count == 0 {
        return Ok(());
    }

    token_usage_checker
        .record_tokens(engineer, token_count)
        .await
        .map_err(AnthropicProxyError::TokenUsageRecordFailed)
}

pub(crate) fn anthropic_usage_from_json_slice(body: &[u8]) -> Option<AnthropicUsage> {
    let value = serde_json::from_slice::<Value>(body).ok()?;

    anthropic_usage_from_json_value(value.get("usage")?)
}

fn anthropic_usage_from_json_value(value: &Value) -> Option<AnthropicUsage> {
    Some(AnthropicUsage {
        cache_creation_input_tokens: value
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64),
        cache_read_input_tokens: value.get("cache_read_input_tokens").and_then(Value::as_u64),
        input_tokens: value.get("input_tokens").and_then(Value::as_u64),
        output_tokens: value.get("output_tokens").and_then(Value::as_u64),
    })
    .filter(AnthropicUsage::has_usage)
}

async fn record_usage_from_json_body(
    token_usage_checker: &TokenUsageChecker,
    engineer: &AuthenticatedEngineer,
    body: &Bytes,
) -> Result<(), AnthropicProxyError> {
    let Some(usage) = anthropic_usage_from_json_slice(body) else {
        warn!("Anthropic response did not include token usage");
        return Ok(());
    };

    record_anthropic_usage(token_usage_checker, engineer, usage).await
}

fn usage_recording_stream(
    provider_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
    token_usage_checker: Arc<TokenUsageChecker>,
    engineer: AuthenticatedEngineer,
    stream_guard: OwnedActiveStreamGuard,
) -> impl Stream<Item = Result<Bytes, Box<dyn Error + Send + Sync>>> {
    stream::unfold(
        (
            provider_stream,
            AnthropicStreamUsageRecorder::new(token_usage_checker, engineer),
            Some(stream_guard),
        ),
        |(mut provider_stream, mut usage_recorder, stream_guard)| async move {
            match provider_stream.next().await {
                Some(Ok(chunk)) => {
                    usage_recorder.observe_chunk(&chunk);

                    Some((Ok(chunk), (provider_stream, usage_recorder, stream_guard)))
                }
                Some(Err(error)) => Some((
                    Err(Box::new(error) as Box<dyn Error + Send + Sync>),
                    (provider_stream, usage_recorder, stream_guard),
                )),
                None => {
                    if let Err(error) = usage_recorder.record_observed_usage().await {
                        warn!(%error, "failed to record Anthropic streaming token usage");
                    }

                    drop(stream_guard);
                    None
                }
            }
        },
    )
}

struct AnthropicStreamUsageRecorder {
    buffered_event: Vec<u8>,
    engineer: AuthenticatedEngineer,
    recording_attempted: bool,
    token_usage_checker: Arc<TokenUsageChecker>,
    usage: AnthropicStreamUsage,
}

impl AnthropicStreamUsageRecorder {
    fn new(token_usage_checker: Arc<TokenUsageChecker>, engineer: AuthenticatedEngineer) -> Self {
        Self {
            buffered_event: Vec::new(),
            engineer,
            recording_attempted: false,
            token_usage_checker,
            usage: AnthropicStreamUsage::default(),
        }
    }

    fn observe_chunk(&mut self, chunk: &[u8]) {
        self.usage.observe_chunk(chunk, &mut self.buffered_event);
    }

    async fn record_observed_usage(&mut self) -> Result<(), AnthropicProxyError> {
        self.recording_attempted = true;

        let Some(usage) = self.usage.observed_usage() else {
            return Ok(());
        };

        record_anthropic_usage(&self.token_usage_checker, &self.engineer, usage).await
    }
}

impl Drop for AnthropicStreamUsageRecorder {
    fn drop(&mut self) {
        if self.recording_attempted {
            return;
        }

        let Some(usage) = self.usage.observed_usage() else {
            return;
        };

        let token_usage_checker = Arc::clone(&self.token_usage_checker);
        let engineer = self.engineer.clone();

        tokio::spawn(async move {
            if let Err(error) = record_anthropic_usage(&token_usage_checker, &engineer, usage).await
            {
                warn!(%error, "failed to record dropped Anthropic streaming token usage");
            }
        });
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AnthropicUsage {
    pub cache_creation_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

impl AnthropicUsage {
    pub(crate) fn total_tokens(&self) -> u64 {
        self.cache_creation_input_tokens.unwrap_or(0)
            + self.cache_read_input_tokens.unwrap_or(0)
            + self.input_tokens.unwrap_or(0)
            + self.output_tokens.unwrap_or(0)
    }

    fn has_usage(&self) -> bool {
        self.cache_creation_input_tokens.is_some()
            || self.cache_read_input_tokens.is_some()
            || self.input_tokens.is_some()
            || self.output_tokens.is_some()
    }
}

#[derive(Default)]
pub(crate) struct AnthropicStreamUsage {
    usage: AnthropicUsage,
}

impl AnthropicStreamUsage {
    pub(crate) fn observe_chunk(&mut self, chunk: &[u8], buffered_event: &mut Vec<u8>) {
        buffered_event.extend_from_slice(chunk);

        while let Some(event_end) = find_sse_event_end(buffered_event) {
            let event = buffered_event.drain(..event_end).collect::<Vec<_>>();
            trim_sse_event_separator(buffered_event);
            self.observe_event(&event);
        }
    }

    pub(crate) fn finish(self) -> Option<AnthropicUsage> {
        self.usage.has_usage().then_some(self.usage)
    }

    fn observed_usage(&self) -> Option<AnthropicUsage> {
        self.usage.has_usage().then_some(self.usage)
    }

    fn observe_event(&mut self, event: &[u8]) {
        let Some(data) = sse_event_data(event) else {
            return;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&data) else {
            return;
        };

        match value.get("type").and_then(Value::as_str) {
            Some("message_start") => {
                if let Some(usage) = value
                    .get("message")
                    .and_then(|message| message.get("usage"))
                    .and_then(anthropic_usage_from_json_value)
                {
                    if usage.cache_creation_input_tokens.is_some() {
                        self.usage.cache_creation_input_tokens = usage.cache_creation_input_tokens;
                    }
                    if usage.cache_read_input_tokens.is_some() {
                        self.usage.cache_read_input_tokens = usage.cache_read_input_tokens;
                    }
                    if usage.input_tokens.is_some() {
                        self.usage.input_tokens = usage.input_tokens;
                    }
                    if usage.output_tokens.is_some() {
                        self.usage.output_tokens = usage.output_tokens;
                    }
                }
            }
            Some("message_delta") => {
                if let Some(usage) = value.get("usage").and_then(anthropic_usage_from_json_value) {
                    if usage.cache_creation_input_tokens.is_some() {
                        self.usage.cache_creation_input_tokens = usage.cache_creation_input_tokens;
                    }
                    if usage.cache_read_input_tokens.is_some() {
                        self.usage.cache_read_input_tokens = usage.cache_read_input_tokens;
                    }
                    if usage.input_tokens.is_some() {
                        self.usage.input_tokens = usage.input_tokens;
                    }
                    if usage.output_tokens.is_some() {
                        self.usage.output_tokens = usage.output_tokens;
                    }
                }
            }
            _ => {}
        }
    }
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

fn copy_response_headers(provider_headers: &HeaderMap, response_headers: Option<&mut HeaderMap>) {
    let Some(response_headers) = response_headers else {
        return;
    };
    let response_connection_headers = ConnectionHeaderNames::from_headers(provider_headers);

    for (name, value) in provider_headers {
        if should_forward_response_header(name, &response_connection_headers) {
            response_headers.append(name, value.clone());
        }
    }
}

pub(crate) fn should_forward_request_header(
    name: &HeaderName,
    connection_headers: &ConnectionHeaderNames,
) -> bool {
    !is_hop_by_hop_header(name)
        && !connection_headers.contains(name)
        && name != HOST
        && name != CONTENT_LENGTH
        && !name.as_str().eq_ignore_ascii_case(INTERNAL_API_KEY_HEADER)
}

pub(crate) fn should_forward_response_header(
    name: &HeaderName,
    connection_headers: &ConnectionHeaderNames,
) -> bool {
    !is_hop_by_hop_header(name) && !connection_headers.contains(name) && name != CONTENT_LENGTH
}

fn is_hop_by_hop_header(name: &HeaderName) -> bool {
    name == CONNECTION
        || name == PROXY_AUTHENTICATE
        || name == PROXY_AUTHORIZATION
        || name == TE
        || name == TRAILER
        || name == TRANSFER_ENCODING
        || name == UPGRADE
}

#[derive(Default)]
pub(crate) struct ConnectionHeaderNames {
    names: Vec<String>,
}

impl ConnectionHeaderNames {
    pub(crate) fn from_headers(headers: &HeaderMap) -> Self {
        let names = headers
            .get_all(CONNECTION)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase)
            .collect();

        Self { names }
    }

    pub(crate) fn contains(&self, name: &HeaderName) -> bool {
        self.names
            .iter()
            .any(|connection_header| name.as_str().eq_ignore_ascii_case(connection_header))
    }
}

pub async fn load_anthropic_proxy(
    secrets_client: &SecretsManagerClient,
    secret_arn: &str,
) -> Result<AnthropicProxy, AnthropicSecretError> {
    let output = secrets_client
        .get_secret_value()
        .secret_id(secret_arn)
        .send()
        .await
        .map_err(|source| AnthropicSecretError::FetchFailed {
            source: Box::new(source),
        })?;

    let api_key = output
        .secret_string()
        .ok_or(AnthropicSecretError::MissingSecretString)?;

    if api_key.is_empty() {
        return Err(AnthropicSecretError::EmptySecretString);
    }

    Ok(AnthropicProxy::new(api_key.to_string()))
}

#[derive(Debug)]
enum AnthropicRouteError {
    Auth(AuthError),
    RateLimit(RateLimitError),
    TokenUsage(TokenUsageError),
    TooManyActiveStreams,
    Proxy(AnthropicProxyError),
}

impl IntoResponse for AnthropicRouteError {
    fn into_response(self) -> Response<Body> {
        let (status, message) = match &self {
            Self::Auth(AuthError::MissingApiKey)
            | Self::Auth(AuthError::InvalidApiKeyFormat)
            | Self::Auth(AuthError::InvalidCredentials)
            | Self::Auth(AuthError::DisabledEngineer) => {
                (StatusCode::UNAUTHORIZED, self.to_string())
            }
            Self::Auth(AuthError::LookupFailed(_)) => {
                error!(error = %self, "Anthropic auth lookup failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "authentication is temporarily unavailable".to_string(),
                )
            }
            Self::RateLimit(RateLimitError::RateLimitExceeded) => {
                (StatusCode::TOO_MANY_REQUESTS, self.to_string())
            }
            Self::RateLimit(_) => {
                error!(error = %self, "Anthropic rate limit check failed");
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
                error!(error = %self, "Anthropic token usage check failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "token usage check is temporarily unavailable".to_string(),
                )
            }
            Self::TooManyActiveStreams => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many active streams".to_string(),
            ),
            Self::Proxy(_) => {
                error!(error = %self, "Anthropic proxy request failed");
                (
                    StatusCode::BAD_GATEWAY,
                    "Anthropic request failed".to_string(),
                )
            }
        };

        (status, axum::Json(json!({ "message": message }))).into_response()
    }
}

impl Display for AnthropicRouteError {
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

impl Error for AnthropicRouteError {
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
pub enum AnthropicProxyError {
    ProviderRequestFailed(reqwest::Error),
    ResponseBuildFailed(axum::http::Error),
    TokenUsageRecordFailed(TokenUsageError),
}

impl Display for AnthropicProxyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderRequestFailed(error) => {
                write!(formatter, "failed to call Anthropic: {error}")
            }
            Self::ResponseBuildFailed(error) => {
                write!(formatter, "failed to build Anthropic response: {error}")
            }
            Self::TokenUsageRecordFailed(error) => {
                write!(formatter, "failed to record Anthropic token usage: {error}")
            }
        }
    }
}

impl Error for AnthropicProxyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ProviderRequestFailed(error) => Some(error),
            Self::ResponseBuildFailed(error) => Some(error),
            Self::TokenUsageRecordFailed(error) => Some(error),
        }
    }
}

#[derive(Debug)]
pub enum AnthropicSecretError {
    FetchFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    MissingSecretString,
    EmptySecretString,
}

impl Display for AnthropicSecretError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FetchFailed { source } => {
                write!(
                    formatter,
                    "failed to fetch Anthropic API key secret: {source}"
                )
            }
            Self::MissingSecretString => {
                write!(
                    formatter,
                    "Anthropic API key secret must contain a string value"
                )
            }
            Self::EmptySecretString => {
                write!(formatter, "Anthropic API key secret must not be empty")
            }
        }
    }
}

impl Error for AnthropicSecretError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::FetchFailed { source } => Some(source.as_ref()),
            Self::MissingSecretString | Self::EmptySecretString => None,
        }
    }
}

#[cfg(test)]
pub(crate) fn test_proxy(api_key: &str) -> AnthropicProxy {
    AnthropicProxy::new(api_key.to_string())
}

#[cfg(test)]
pub(crate) fn test_header(value: &'static str) -> HeaderName {
    HeaderName::from_static(value)
}
