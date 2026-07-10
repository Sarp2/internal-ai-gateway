use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::{
    CONNECTION, CONTENT_LENGTH, HOST, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION, TE, TRAILER,
    TRANSFER_ENCODING, UPGRADE,
};
use axum::http::{HeaderMap, HeaderName, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use reqwest::Client as HttpClient;
use reqwest::redirect::Policy;
use serde_json::json;
use tracing::error;

use crate::app::AppState;
use crate::auth::AuthError;
use crate::rate_limit::RateLimitError;
use crate::streams::OwnedActiveStreamGuard;
use crate::token_usage::TokenUsageError;

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

        let mut response_builder = Response::builder().status(
            StatusCode::from_u16(provider_response.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY),
        );

        copy_response_headers(provider_response.headers(), response_builder.headers_mut());

        let stream = provider_response.bytes_stream().map(move |chunk| {
            let _guard = &stream_guard;
            chunk.map_err(|error| Box::new(error) as Box<dyn Error + Send + Sync>)
        });

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
        .forward_messages(request, stream_guard)
        .await
        .map_err(AnthropicRouteError::Proxy)
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
        }
    }
}

impl Error for AnthropicProxyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ProviderRequestFailed(error) => Some(error),
            Self::ResponseBuildFailed(error) => Some(error),
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
