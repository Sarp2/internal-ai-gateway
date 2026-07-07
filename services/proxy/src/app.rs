use std::sync::Arc;

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::auth::RequestAuthenticator;
use crate::health::health;
use crate::rate_limit::RateLimiter;

#[derive(Clone)]
pub struct AppState {
    _authenticator: Arc<RequestAuthenticator>,
    _rate_limiter: Arc<RateLimiter>,
}

impl AppState {
    pub fn new(authenticator: Arc<RequestAuthenticator>, rate_limiter: Arc<RateLimiter>) -> Self {
        Self {
            _authenticator: authenticator,
            _rate_limiter: rate_limiter,
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
