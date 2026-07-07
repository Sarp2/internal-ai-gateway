use std::net::SocketAddr;
use std::sync::Arc;

use aws_config::BehaviorVersion;
use aws_sdk_cloudwatch::Client as CloudWatchClient;
use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use tokio::net::TcpListener;
use tracing::info;

use crate::api_key::load_api_key_hasher;
use crate::app::{AppState, app};
use crate::auth::RequestAuthenticator;
use crate::config::ProxyConfig;
use crate::engineer_auth::EngineerAuth;
use crate::metrics::start_active_stream_metric_publisher;
use crate::shutdown::shutdown_signal;
use crate::streams::ActiveStreamTracker;
use crate::telemetry::init_tracing;

pub mod api_key;
mod app;
pub mod auth;
mod config;
pub mod engineer_auth;
mod health;
mod metrics;
mod shutdown;
pub mod streams;
mod telemetry;

#[cfg(test)]
mod api_key_test;
#[cfg(test)]
mod app_test;
#[cfg(test)]
mod auth_test;
#[cfg(test)]
mod config_test;
#[cfg(test)]
mod engineer_auth_test;
#[cfg(test)]
mod streams_test;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let config = ProxyConfig::from_env()?;
    let aws_config = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let engineer_auth = Arc::new(EngineerAuth::new(
        DynamoDbClient::new(&aws_config),
        config.engineers_table_name.clone(),
        config.engineers_api_key_index_name.clone(),
    ));
    let api_key_hasher = Arc::new(
        load_api_key_hasher(
            &SecretsManagerClient::new(&aws_config),
            &config.proxy_api_key_hash_secret_arn,
        )
        .await?,
    );
    let authenticator = Arc::new(RequestAuthenticator::new(api_key_hasher, engineer_auth));
    let stream_tracker = Arc::new(ActiveStreamTracker::new(config.max_active_streams));
    // TODO: Wire this tracker into the streaming proxy route so ActiveStreams reflects
    // live streams and MAX_ACTIVE_STREAMS is enforced for real proxy traffic.
    start_active_stream_metric_publisher(
        Arc::clone(&stream_tracker),
        config.metric_interval,
        CloudWatchClient::new(&aws_config),
    );

    let address = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = TcpListener::bind(address).await?;

    info!(%address, "proxy service listening");

    axum::serve(listener, app(AppState::new(authenticator)))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}
