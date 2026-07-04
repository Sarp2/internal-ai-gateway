use std::env;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use aws_config::BehaviorVersion;
use aws_sdk_cloudwatch::Client as CloudWatchClient;
use aws_sdk_cloudwatch::types::{Dimension, MetricDatum, StandardUnit};
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::MissedTickBehavior;
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_PORT: u16 = 8080;
const DEFAULT_MAX_ACTIVE_STREAMS: usize = 200;
const DEFAULT_METRIC_INTERVAL_SECONDS: u64 = 15;
const METRIC_NAMESPACE: &str = "InternalAiGateway/Proxy";
const ACTIVE_STREAMS_METRIC_NAME: &str = "ActiveStreams";
const SERVICE_NAME: &str = "internal-ai-gateway-proxy";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let config = ProxyConfig::from_env();
    let stream_tracker = Arc::new(ActiveStreamTracker::new(config.max_active_streams));
    start_active_stream_metric_publisher(
        Arc::clone(&stream_tracker),
        config.metric_interval,
        CloudWatchClient::new(&aws_config::load_defaults(BehaviorVersion::latest()).await),
    );

    let port = config.port;
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(address).await?;

    info!(%address, "proxy service listening");

    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

struct ProxyConfig {
    port: u16,
    max_active_streams: usize,
    metric_interval: Duration,
}

impl ProxyConfig {
    fn from_env() -> Self {
        Self {
            port: parse_env("PORT", DEFAULT_PORT),
            max_active_streams: parse_env("MAX_ACTIVE_STREAMS", DEFAULT_MAX_ACTIVE_STREAMS),
            metric_interval: Duration::from_secs(parse_env(
                "ACTIVE_STREAM_METRIC_INTERVAL_SECONDS",
                DEFAULT_METRIC_INTERVAL_SECONDS,
            )),
        }
    }
}

fn parse_env<T>(name: &str, default_value: T) -> T
where
    T: std::str::FromStr,
{
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<T>().ok())
        .unwrap_or(default_value)
}

fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .fallback(not_found)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn not_found(_request: Request<Body>) -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "message": "Route not found." })),
    )
}

pub struct ActiveStreamTracker {
    active_streams: AtomicUsize,
    max_active_streams: usize,
}

impl ActiveStreamTracker {
    pub fn new(max_active_streams: usize) -> Self {
        Self {
            active_streams: AtomicUsize::new(0),
            max_active_streams,
        }
    }

    pub fn current(&self) -> usize {
        self.active_streams.load(Ordering::Relaxed)
    }

    pub fn try_start_stream(&self) -> Option<ActiveStreamGuard<'_>> {
        let mut current = self.active_streams.load(Ordering::Relaxed);

        loop {
            if current >= self.max_active_streams {
                return None;
            }

            match self.active_streams.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(ActiveStreamGuard {
                        tracker: self,
                        released: false,
                    });
                }
                Err(updated_current) => current = updated_current,
            }
        }
    }

    fn end_stream(&self) {
        self.active_streams.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct ActiveStreamGuard<'a> {
    tracker: &'a ActiveStreamTracker,
    released: bool,
}

impl ActiveStreamGuard<'_> {
    pub fn finish(mut self) {
        self.release();
    }

    fn release(&mut self) {
        if !self.released {
            self.tracker.end_stream();
            self.released = true;
        }
    }
}

impl Drop for ActiveStreamGuard<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

fn start_active_stream_metric_publisher(
    stream_tracker: Arc<ActiveStreamTracker>,
    interval_duration: Duration,
    cloudwatch_client: CloudWatchClient,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(interval_duration);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            interval.tick().await;

            if let Err(error) =
                publish_active_stream_metric(&cloudwatch_client, stream_tracker.current()).await
            {
                tracing::warn!(%error, "failed to publish active stream metric");
            }
        }
    });
}

async fn publish_active_stream_metric(
    cloudwatch_client: &CloudWatchClient,
    active_streams: usize,
) -> Result<(), aws_sdk_cloudwatch::Error> {
    let metric = MetricDatum::builder()
        .metric_name(ACTIVE_STREAMS_METRIC_NAME)
        .unit(StandardUnit::Count)
        .value(active_streams as f64)
        .storage_resolution(1)
        .dimensions(
            Dimension::builder()
                .name("ServiceName")
                .value(SERVICE_NAME)
                .build(),
        )
        .build();

    cloudwatch_client
        .put_metric_data()
        .namespace(METRIC_NAMESPACE)
        .metric_data(metric)
        .send()
        .await?;

    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "failed to install ctrl+c handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "failed to install terminate signal handler");
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

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

    #[test]
    fn active_stream_tracker_counts_active_streams() {
        let tracker = ActiveStreamTracker::new(2);

        let first_stream = tracker
            .try_start_stream()
            .expect("first stream should start");
        let second_stream = tracker
            .try_start_stream()
            .expect("second stream should start");

        assert_eq!(tracker.current(), 2);

        second_stream.finish();
        assert_eq!(tracker.current(), 1);

        drop(first_stream);
        assert_eq!(tracker.current(), 0);
    }

    #[test]
    fn active_stream_tracker_rejects_streams_at_limit() {
        let tracker = ActiveStreamTracker::new(1);
        let _stream = tracker
            .try_start_stream()
            .expect("first stream should start");

        assert!(tracker.try_start_stream().is_none());
        assert_eq!(tracker.current(), 1);
    }
}
