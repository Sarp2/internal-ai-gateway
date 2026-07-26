use axum::Json;
use axum::Router;
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::anthropic::messages;

pub fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/messages", post(messages))
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "healthy" }))
}
