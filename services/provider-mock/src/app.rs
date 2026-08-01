use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::anthropic::{MAX_REQUEST_BODY_BYTES as ANTHROPIC_MAX_REQUEST_BODY_BYTES, messages};
use crate::openai::{MAX_REQUEST_BODY_BYTES as OPENAI_MAX_REQUEST_BODY_BYTES, chat_completions};

pub fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route(
            "/v1/messages",
            post(messages).layer(DefaultBodyLimit::max(ANTHROPIC_MAX_REQUEST_BODY_BYTES)),
        )
        .route(
            "/v1/chat/completions",
            post(chat_completions).layer(DefaultBodyLimit::max(OPENAI_MAX_REQUEST_BODY_BYTES)),
        )
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "healthy" }))
}
