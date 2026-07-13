use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::{ACCEPT_ENCODING, AUTHORIZATION};
use axum::http::{HeaderMap, HeaderName, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures_util::stream;
use futures_util::{Stream, StreamExt};
use reqwest::Client as HttpClient;
use reqwest::redirect::Policy;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::anthropic::{
    ConnectionHeaderNames, should_forward_request_header, should_forward_response_header,
};
use crate::app::AppState;
use crate::auth::AuthError;
use crate::background_tasks::BackgroundTasks;
use crate::engineer_auth::AuthenticatedEngineer;
use crate::openai_request::{OpenAiRequestTransformError, prepare_openai_request};
use crate::rate_limit::RateLimitError;
use crate::streams::OwnedActiveStreamGuard;
use crate::token_reservation::{TokenReservation, TokenReservationError, TokenReservationManager};

const OPENAI_CHAT_COMPLETIONS_URL: &str = "https://api.openai.com/v1/chat/completions";
const OPENAI_ORGANIZATION_HEADER: &str = "openai-organization";
const OPENAI_PROJECT_HEADER: &str = "openai-project";
const STREAM_CHANNEL_CAPACITY: usize = 8;

type ProxyStreamItem = Result<Bytes, Box<dyn Error + Send + Sync>>;

#[derive(Clone)]
pub struct OpenAiProxy {
    api_key: Arc<str>,
    default_max_completion_tokens: u64,
    http_client: HttpClient,
}

impl OpenAiProxy {
    pub fn new(api_key: impl Into<Arc<str>>, default_max_completion_tokens: u64) -> Self {
        Self {
            api_key: api_key.into(),
            default_max_completion_tokens: default_max_completion_tokens.max(1),
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
        token_reservation_manager: Arc<TokenReservationManager>,
        stream_guard: OwnedActiveStreamGuard,
        background_tasks: BackgroundTasks,
    ) -> Result<Response<Body>, OpenAiProxyError> {
        let (parts, body) = request.into_parts();
        let prepared_request = prepare_openai_request(body, self.default_max_completion_tokens)
            .await
            .map_err(OpenAiProxyError::RequestTransform)?;

        let (provider_body, streaming_request, token_budget) = prepared_request.into_parts();

        let reservation = token_reservation_manager
            .reserve(engineer.clone(), token_budget)
            .await
            .map_err(OpenAiProxyError::TokenReservation)?;
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

        let provider_response = match provider_request.body(provider_body).send().await {
            Ok(response) => response,
            Err(error) => {
                if let Err(reconciliation_error) = reservation.reconcile(None).await {
                    warn!(%reconciliation_error, "failed to finalize OpenAI reservation after provider failure");
                }
                return Err(OpenAiProxyError::ProviderRequest(error));
            }
        };

        let is_streaming_response =
            should_stream_provider_response(streaming_request, provider_response.status());

        let mut response_builder = Response::builder().status(
            StatusCode::from_u16(provider_response.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY),
        );

        copy_response_headers(provider_response.headers(), response_builder.headers_mut());

        if !is_streaming_response {
            let body = match provider_response.bytes().await {
                Ok(body) => body,
                Err(error) => {
                    if let Err(reconciliation_error) = reservation.reconcile(None).await {
                        warn!(%reconciliation_error, "failed to finalize OpenAI reservation after response failure");
                    }
                    return Err(OpenAiProxyError::ProviderRequest(error));
                }
            };

            let actual_tokens = openai_usage_from_json_slice(&body).map(|usage| usage.total_tokens);

            if let Err(error) = reservation.reconcile(actual_tokens).await {
                warn!(%error, "failed to reconcile non-streaming OpenAI token usage");
            }

            return response_builder
                .body(Body::from(body))
                .map_err(OpenAiProxyError::ResponseBuild);
        }

        let stream = usage_recording_stream(
            provider_response.bytes_stream(),
            reservation,
            stream_guard,
            background_tasks,
        );

        response_builder
            .body(Body::from_stream(stream))
            .map_err(OpenAiProxyError::ResponseBuild)
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

    let stream_guard = state
        .stream_tracker
        .try_start_owned()
        .ok_or(OpenAiRouteError::TooManyActiveStreams)?;

    state
        .openai_proxy
        .forward_chat_completions(
            request,
            engineer,
            state.token_accounting.reservation_manager(),
            stream_guard,
            state.background_tasks.clone(),
        )
        .await
        .map_err(OpenAiRouteError::Proxy)
}

fn should_forward_openai_request_header(
    name: &HeaderName,
    connection_headers: &ConnectionHeaderNames,
) -> bool {
    should_forward_request_header(name, connection_headers)
        && name != AUTHORIZATION
        && name != ACCEPT_ENCODING
        && name.as_str() != OPENAI_ORGANIZATION_HEADER
        && name.as_str() != OPENAI_PROJECT_HEADER
}

fn should_stream_provider_response(streaming_request: bool, status: StatusCode) -> bool {
    streaming_request && status.is_success()
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

fn usage_recording_stream(
    provider_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
    reservation: TokenReservation,
    stream_guard: OwnedActiveStreamGuard,
    background_tasks: BackgroundTasks,
) -> impl Stream<Item = ProxyStreamItem> {
    let (sender, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
    let engineer_id = reservation.engineer_id().to_string();

    background_tasks.spawn(async move {
        let mut provider_stream = provider_stream;
        let mut usage_recorder = OpenAiStreamUsageRecorder::new(reservation);
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
    recording_attempted: bool,
    reservation: Option<TokenReservation>,
    usage: Option<OpenAiUsage>,
}

impl OpenAiStreamUsageRecorder {
    fn new(reservation: TokenReservation) -> Self {
        Self {
            buffered_event: Vec::new(),
            recording_attempted: false,
            reservation: Some(reservation),
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

        let actual_tokens = self.usage.map(|usage| usage.total_tokens);
        if actual_tokens.is_none() {
            warn!("OpenAI stream ended without a final usage chunk; charging reservation");
        }

        self.reservation
            .take()
            .expect("OpenAI reservation should only be reconciled once")
            .reconcile(actual_tokens)
            .await
            .map_err(OpenAiProxyError::TokenReservation)
    }
}

impl Drop for OpenAiStreamUsageRecorder {
    fn drop(&mut self) {
        if self.recording_attempted {
            return;
        }

        let actual_tokens = self.usage.map(|usage| usage.total_tokens);
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!(
                "failed to reconcile dropped OpenAI stream: no Tokio runtime; reservation remains charged"
            );
            return;
        };

        handle.spawn(async move {
            if let Err(error) = reservation.reconcile(actual_tokens).await {
                warn!(%error, "failed to reconcile dropped OpenAI streaming token usage");
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
    default_max_completion_tokens: u64,
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

    Ok(OpenAiProxy::new(
        api_key.to_string(),
        default_max_completion_tokens,
    ))
}

#[derive(Debug)]
enum OpenAiRouteError {
    Auth(AuthError),
    RateLimit(RateLimitError),
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
            Self::Proxy(OpenAiProxyError::TokenReservation(
                TokenReservationError::LimitExceeded,
            )) => (StatusCode::PAYMENT_REQUIRED, self.to_string()),
            Self::Proxy(OpenAiProxyError::TokenReservation(_)) => {
                error!(error = %self, "OpenAI token reservation failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "token reservation is temporarily unavailable".to_string(),
                )
            }
            Self::TooManyActiveStreams => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many active streams".to_string(),
            ),
            Self::Proxy(OpenAiProxyError::RequestTransform(error))
                if error.is_request_too_large() =>
            {
                (StatusCode::PAYLOAD_TOO_LARGE, self.to_string())
            }
            Self::Proxy(OpenAiProxyError::RequestTransform(error)) if error.is_client_error() => {
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
            Self::Proxy(error) => Some(error),
            Self::TooManyActiveStreams => None,
        }
    }
}

#[derive(Debug)]
enum OpenAiProxyError {
    ProviderRequest(reqwest::Error),
    RequestTransform(OpenAiRequestTransformError),
    ResponseBuild(axum::http::Error),
    TokenReservation(TokenReservationError),
}

impl Display for OpenAiProxyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderRequest(error) => {
                write!(formatter, "failed to call OpenAI: {error}")
            }
            Self::RequestTransform(error) => {
                write!(formatter, "failed to transform OpenAI request: {error}")
            }
            Self::ResponseBuild(error) => {
                write!(formatter, "failed to build OpenAI response: {error}")
            }
            Self::TokenReservation(error) => {
                write!(formatter, "OpenAI token reservation failed: {error}")
            }
        }
    }
}

impl Error for OpenAiProxyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ProviderRequest(error) => Some(error),
            Self::RequestTransform(error) => Some(error),
            Self::ResponseBuild(error) => Some(error),
            Self::TokenReservation(error) => Some(error),
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
    OpenAiProxy::new(api_key.to_string(), 32_768)
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
pub(crate) fn streams_provider_response(streaming_request: bool, status: StatusCode) -> bool {
    should_stream_provider_response(streaming_request, status)
}

#[cfg(test)]
pub(crate) fn test_usage_recording_stream(
    provider_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
    reservation: TokenReservation,
    stream_guard: OwnedActiveStreamGuard,
    background_tasks: BackgroundTasks,
) -> impl Stream<Item = ProxyStreamItem> {
    usage_recording_stream(provider_stream, reservation, stream_guard, background_tasks)
}
