use axum::Json;
use axum::Router;
use axum::routing::get;
use serde_json::{Value, json};

pub fn app() -> Router {
    Router::new().route("/health", get(health))
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "healthy" }))
}
