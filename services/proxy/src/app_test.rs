use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::app::app;

#[tokio::test]
async fn returns_healthy_status_from_health_route() {
    let response = app()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/health")
                .body(Body::empty())
                .expect("health request should build"),
        )
        .await
        .expect("health request should complete");

    assert_eq!(response.status(), StatusCode::OK);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("health body should be readable");

    assert_eq!(&body[..], br#"{"status":"ok"}"#);
}

#[tokio::test]
async fn returns_not_found_for_unknown_routes() {
    let response = app()
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/unknown")
                .body(Body::empty())
                .expect("unknown request should build"),
        )
        .await
        .expect("unknown request should complete");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("not found body should be readable");

    assert_eq!(&body[..], br#"{"message":"Route not found."}"#);
}
