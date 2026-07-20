use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use axum::body::Body;
use axum::body::Bytes;
use axum::extract::State;
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
use tokio::sync::mpsc;
use tracing::{error, warn};

use crate::anthropic_request::{AnthropicRequestError, prepare_anthropic_request};
use crate::app::AppState;
use crate::auth::AuthError;
use crate::background_tasks::BackgroundTasks;
use crate::engineer_auth::AuthenticatedEngineer;
use crate::provider_url::provider_url;
use crate::rate_limit::RateLimitError;
use crate::sse::{event_data as sse_event_data, take_next_event};
use crate::streams::OwnedActiveStreamGuard;
use crate::token_reservation::{TokenReservation, TokenReservationError, TokenReservationManager};

const INTERNAL_API_KEY_HEADER: &str = "x-api-key";
const STREAM_CHANNEL_CAPACITY: usize = 8;

type ProxyStreamItem = Result<Bytes, Box<dyn Error + Send + Sync>>;

#[derive(Clone)]
pub struct AnthropicProxy {
    api_key: Arc<str>,
    http_client: HttpClient,
    messages_url: Arc<str>,
}

impl AnthropicProxy {
    pub fn new(api_key: impl Into<Arc<str>>, base_url: &str) -> Self {
        Self {
            api_key: api_key.into(),
            http_client: HttpClient::builder()
                .connect_timeout(Duration::from_secs(10))
                .pool_idle_timeout(Duration::from_secs(90))
                .read_timeout(Duration::from_secs(300))
                .redirect(Policy::none())
                .build()
                .expect("Anthropic HTTP client configuration should be valid"),
            messages_url: provider_url(base_url, "/v1/messages").into(),
        }
    }

    async fn forward_messages(
        &self,
        request: Request<Body>,
        engineer: AuthenticatedEngineer,
        token_reservation_manager: Arc<TokenReservationManager>,
        background_tasks: BackgroundTasks,
        stream_guard: OwnedActiveStreamGuard,
    ) -> Result<Response<Body>, AnthropicProxyError> {
        let (parts, body) = request.into_parts();
        let prepared_request = prepare_anthropic_request(body)
            .await
            .map_err(AnthropicProxyError::RequestPreparation)?;
        let (provider_body, streaming_request, token_budget) = prepared_request.into_parts();
        let reservation = token_reservation_manager
            .reserve(engineer, token_budget)
            .await
            .map_err(AnthropicProxyError::TokenReservation)?;
        let request_connection_headers = ConnectionHeaderNames::from_headers(&parts.headers);
        let mut provider_request = self
            .http_client
            .post(self.messages_url.as_ref())
            .header(INTERNAL_API_KEY_HEADER, self.api_key.as_ref());

        for (name, value) in parts.headers.iter() {
            if should_forward_request_header(name, &request_connection_headers) {
                provider_request = provider_request.header(name, value);
            }
        }

        let provider_response = match provider_request.body(provider_body).send().await {
            Ok(response) => response,
            Err(error) => {
                if let Err(reconciliation_error) = reservation.reconcile(None).await {
                    warn!(%reconciliation_error, "failed to finalize Anthropic reservation after provider failure");
                }
                return Err(AnthropicProxyError::ProviderRequestFailed(error));
            }
        };

        let is_streaming_response = streaming_request && provider_response.status().is_success();
        let provider_status = provider_response.status();
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
                        warn!(%reconciliation_error, "failed to finalize Anthropic reservation after response failure");
                    }
                    return Err(AnthropicProxyError::ProviderRequestFailed(error));
                }
            };
            let actual_tokens = completed_response_tokens(provider_status, &body);
            if let Err(error) = reservation.reconcile(actual_tokens).await {
                warn!(%error, "failed to reconcile non-streaming Anthropic token usage");
            }

            return response_builder
                .body(Body::from(body))
                .map_err(AnthropicProxyError::ResponseBuildFailed);
        }

        let stream = usage_recording_stream(
            provider_response.bytes_stream(),
            reservation,
            background_tasks,
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

    let stream_guard = state
        .stream_tracker
        .try_start_owned()
        .ok_or(AnthropicRouteError::TooManyActiveStreams)?;
    state
        .anthropic_proxy
        .forward_messages(
            request,
            engineer,
            state.token_accounting.reservation_manager(),
            state.background_tasks.clone(),
            stream_guard,
        )
        .await
        .map_err(AnthropicRouteError::Proxy)
}

pub(crate) fn anthropic_usage_from_json_slice(body: &[u8]) -> Option<AnthropicUsage> {
    let value = serde_json::from_slice::<Value>(body).ok()?;

    anthropic_usage_from_json_value(value.get("usage")?)
}

fn completed_response_tokens(status: reqwest::StatusCode, body: &[u8]) -> Option<u64> {
    if status.is_client_error() {
        return Some(0);
    }

    anthropic_usage_from_json_slice(body).map(|usage| usage.total_tokens())
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

fn usage_recording_stream(
    provider_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
    reservation: TokenReservation,
    background_tasks: BackgroundTasks,
    stream_guard: OwnedActiveStreamGuard,
) -> impl Stream<Item = ProxyStreamItem> {
    let (sender, receiver) = mpsc::channel(STREAM_CHANNEL_CAPACITY);
    let engineer_id = reservation.engineer_id().to_string();
    let cancellation = background_tasks.cancellation_token();

    background_tasks.spawn(async move {
        let mut provider_stream = provider_stream;
        let mut usage_recorder = AnthropicStreamUsageRecorder::new(reservation);
        let mut downstream_connected = true;
        let _stream_guard = stream_guard;

        'provider: loop {
            let provider_result = tokio::select! {
                () = cancellation.cancelled() => {
                    warn!(%engineer_id, "Anthropic provider drain cancelled during shutdown");
                    break;
                }
                provider_result = provider_stream.next() => provider_result,
            };
            let Some(provider_result) = provider_result else {
                break;
            };

            match provider_result {
                Ok(chunk) => {
                    usage_recorder.observe_chunk(&chunk);
                    if downstream_connected {
                        let send_result = tokio::select! {
                            () = cancellation.cancelled() => {
                                warn!(%engineer_id, "Anthropic response forwarding cancelled during shutdown");
                                break 'provider;
                            }
                            send_result = sender.send(Ok(chunk)) => send_result,
                        };
                        if send_result.is_err() {
                            downstream_connected = false;
                            warn!(%engineer_id, "Anthropic client disconnected; continuing provider drain");
                        }
                    }
                }
                Err(error) => {
                    warn!(%engineer_id, %error, "Anthropic provider stream failed");
                    if downstream_connected {
                        tokio::select! {
                            () = cancellation.cancelled() => {
                                warn!(%engineer_id, "Anthropic error forwarding cancelled during shutdown");
                            }
                            _ = sender.send(Err(Box::new(error) as Box<dyn Error + Send + Sync>)) => {}
                        }
                    }
                    break;
                }
            }
        }

        if let Err(error) = usage_recorder.record_observed_usage().await {
            warn!(%engineer_id, %error, "failed to reconcile Anthropic streaming token usage");
        }
    });

    stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    })
}

struct AnthropicStreamUsageRecorder {
    buffered_event: Vec<u8>,
    recording_attempted: bool,
    reservation: Option<TokenReservation>,
    usage: AnthropicStreamUsage,
}

impl AnthropicStreamUsageRecorder {
    fn new(reservation: TokenReservation) -> Self {
        Self {
            buffered_event: Vec::new(),
            recording_attempted: false,
            reservation: Some(reservation),
            usage: AnthropicStreamUsage::default(),
        }
    }

    fn observe_chunk(&mut self, chunk: &[u8]) {
        self.usage.observe_chunk(chunk, &mut self.buffered_event);
    }

    async fn record_observed_usage(&mut self) -> Result<(), AnthropicProxyError> {
        self.recording_attempted = true;

        let actual_tokens = self
            .usage
            .observed_usage()
            .map(|usage| usage.total_tokens());
        if actual_tokens.is_none() {
            warn!("Anthropic stream ended without final usage; charging reservation");
        }
        self.reservation
            .take()
            .expect("Anthropic reservation should only be reconciled once")
            .reconcile(actual_tokens)
            .await
            .map_err(AnthropicProxyError::TokenReservation)
    }
}

impl Drop for AnthropicStreamUsageRecorder {
    fn drop(&mut self) {
        if self.recording_attempted {
            return;
        }

        let Some(reservation) = self.reservation.take() else {
            return;
        };
        let actual_tokens = self
            .usage
            .observed_usage()
            .map(|usage| usage.total_tokens());
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            warn!(
                "failed to reconcile dropped Anthropic stream: no Tokio runtime; reservation remains charged"
            );
            return;
        };
        handle.spawn(async move {
            if let Err(error) = reservation.reconcile(actual_tokens).await {
                warn!(%error, "failed to reconcile dropped Anthropic streaming token usage");
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

        while let Some(event) = take_next_event(buffered_event) {
            self.observe_event(&event);
        }
    }

    pub(crate) fn observed_usage(&self) -> Option<AnthropicUsage> {
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
    base_url: &str,
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

    Ok(AnthropicProxy::new(api_key.to_string(), base_url))
}

#[derive(Debug)]
enum AnthropicRouteError {
    Auth(AuthError),
    RateLimit(RateLimitError),
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
            Self::Proxy(AnthropicProxyError::TokenReservation(
                TokenReservationError::LimitExceeded,
            )) => (StatusCode::PAYMENT_REQUIRED, self.to_string()),
            Self::Proxy(AnthropicProxyError::TokenReservation(_)) => {
                error!(error = %self, "Anthropic token reservation failed");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "token reservation is temporarily unavailable".to_string(),
                )
            }
            Self::TooManyActiveStreams => (
                StatusCode::TOO_MANY_REQUESTS,
                "too many active streams".to_string(),
            ),
            Self::Proxy(AnthropicProxyError::RequestPreparation(error))
                if error.is_request_too_large() =>
            {
                (StatusCode::PAYLOAD_TOO_LARGE, self.to_string())
            }
            Self::Proxy(AnthropicProxyError::RequestPreparation(error))
                if error.is_upload_timeout() =>
            {
                (StatusCode::REQUEST_TIMEOUT, self.to_string())
            }
            Self::Proxy(AnthropicProxyError::RequestPreparation(error))
                if error.is_client_error() =>
            {
                (StatusCode::BAD_REQUEST, self.to_string())
            }
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
            Self::Proxy(error) => Some(error),
            Self::TooManyActiveStreams => None,
        }
    }
}

#[derive(Debug)]
pub(crate) enum AnthropicProxyError {
    ProviderRequestFailed(reqwest::Error),
    RequestPreparation(AnthropicRequestError),
    ResponseBuildFailed(axum::http::Error),
    TokenReservation(TokenReservationError),
}

impl Display for AnthropicProxyError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderRequestFailed(error) => {
                write!(formatter, "failed to call Anthropic: {error}")
            }
            Self::RequestPreparation(error) => {
                write!(formatter, "failed to prepare Anthropic request: {error}")
            }
            Self::ResponseBuildFailed(error) => {
                write!(formatter, "failed to build Anthropic response: {error}")
            }
            Self::TokenReservation(error) => {
                write!(formatter, "Anthropic token reservation failed: {error}")
            }
        }
    }
}

impl Error for AnthropicProxyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ProviderRequestFailed(error) => Some(error),
            Self::RequestPreparation(error) => Some(error),
            Self::ResponseBuildFailed(error) => Some(error),
            Self::TokenReservation(error) => Some(error),
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
    AnthropicProxy::new(api_key.to_string(), "https://api.anthropic.com")
}

#[cfg(test)]
pub(crate) fn test_header(value: &'static str) -> HeaderName {
    HeaderName::from_static(value)
}

#[cfg(test)]
pub(crate) fn completed_tokens(status: u16, body: &[u8]) -> Option<u64> {
    completed_response_tokens(
        reqwest::StatusCode::from_u16(status).expect("test status should be valid"),
        body,
    )
}

#[cfg(test)]
pub(crate) fn test_usage_recording_stream(
    provider_stream: impl Stream<Item = Result<Bytes, reqwest::Error>> + Unpin + Send + 'static,
    reservation: TokenReservation,
    background_tasks: BackgroundTasks,
    stream_guard: OwnedActiveStreamGuard,
) -> impl Stream<Item = ProxyStreamItem> {
    usage_recording_stream(provider_stream, reservation, background_tasks, stream_guard)
}
