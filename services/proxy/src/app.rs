use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::anthropic::{AnthropicProxy, messages};
use crate::auth::RequestAuthenticator;
use crate::background_tasks::BackgroundTasks;
use crate::health::health;
use crate::openai::{OpenAiProxy, chat_completions};
use crate::rate_limit::RateLimiter;
use crate::streams::ActiveStreamTracker;
use crate::token_usage::TokenUsageChecker;

#[derive(Clone)]
pub struct AppState {
    pub(crate) anthropic_proxy: Arc<AnthropicProxy>,
    pub(crate) authenticator: Arc<RequestAuthenticator>,
    pub(crate) background_tasks: BackgroundTasks,
    pub(crate) openai_proxy: Arc<OpenAiProxy>,
    pub(crate) rate_limiter: Arc<RateLimiter>,
    pub(crate) stream_tracker: Arc<ActiveStreamTracker>,
    pub(crate) token_usage_checker: Arc<TokenUsageChecker>,
}

impl AppState {
    pub fn new(
        anthropic_proxy: Arc<AnthropicProxy>,
        authenticator: Arc<RequestAuthenticator>,
        background_tasks: BackgroundTasks,
        openai_proxy: Arc<OpenAiProxy>,
        rate_limiter: Arc<RateLimiter>,
        stream_tracker: Arc<ActiveStreamTracker>,
        token_usage_checker: Arc<TokenUsageChecker>,
    ) -> Self {
        Self {
            anthropic_proxy,
            authenticator,
            background_tasks,
            openai_proxy,
            rate_limiter,
            stream_tracker,
            token_usage_checker,
        }
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/anthropic/messages", post(messages))
        .route("/v1/openai/chat/completions", post(chat_completions))
        .fallback(not_found)
        .with_state(state)
}

async fn not_found(_request: Request<Body>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "message": "Route not found." })),
    )
}
