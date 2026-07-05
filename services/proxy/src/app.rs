use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::auth::ApiKeyHasher;
use crate::health::health;

#[derive(Clone)]
pub struct AppState {
    _api_key_hasher: Arc<ApiKeyHasher>,
}

impl AppState {
    pub fn new(api_key_hasher: Arc<ApiKeyHasher>) -> Self {
        Self {
            _api_key_hasher: api_key_hasher,
        }
    }
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .fallback(not_found)
        .with_state(state)
}

async fn not_found(_request: Request<Body>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "message": "Route not found." })),
    )
}
