use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::anthropic::{MAX_REQUEST_BODY_BYTES, messages};

pub fn app() -> Router {
    Router::new().route("/health", get(health)).route(
        "/v1/messages",
        post(messages).layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES)),
    )
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "healthy" }))
}
