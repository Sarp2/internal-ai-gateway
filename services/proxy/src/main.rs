use std::net::SocketAddr;
use std::sync::Arc;

use aws_config::BehaviorVersion;
use aws_sdk_cloudwatch::Client as CloudWatchClient;
use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_secretsmanager::Client as SecretsManagerClient;
use tokio::net::TcpListener;
use tracing::info;

use crate::anthropic::load_anthropic_proxy;
use crate::api_key::load_api_key_hasher;
use crate::app::{AppState, app};
use crate::auth::RequestAuthenticator;
use crate::config::ProxyConfig;
use crate::engineer_auth::EngineerAuth;
use crate::metrics::start_active_stream_metric_publisher;
use crate::openai::load_openai_proxy;
use crate::rate_limit::RateLimiter;
use crate::shutdown::shutdown_signal;
use crate::streams::ActiveStreamTracker;
use crate::telemetry::init_tracing;
use crate::token_usage::TokenUsageChecker;

pub mod anthropic;
pub mod api_key;
mod app;
pub mod auth;
mod config;
pub mod engineer_auth;
mod health;
mod metrics;
pub mod openai;
pub mod rate_limit;
mod shutdown;
pub mod streams;
mod telemetry;
pub mod token_usage;

#[cfg(test)]
mod anthropic_test;
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
mod openai_test;
#[cfg(test)]
mod rate_limit_test;
#[cfg(test)]
mod streams_test;
#[cfg(test)]
mod token_usage_test;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let config = ProxyConfig::from_env()?;
    let aws_config = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let secrets_client = SecretsManagerClient::new(&aws_config);
    let engineer_auth = Arc::new(EngineerAuth::new(
        DynamoDbClient::new(&aws_config),
        config.engineers_table_name.clone(),
        config.engineers_api_key_index_name.clone(),
    ));
    let api_key_hasher = Arc::new(
        load_api_key_hasher(&secrets_client, &config.proxy_api_key_hash_secret_arn).await?,
    );
    let anthropic_proxy = Arc::new(
        load_anthropic_proxy(&secrets_client, &config.anthropic_api_key_secret_arn).await?,
    );
    let openai_proxy =
        Arc::new(load_openai_proxy(&secrets_client, &config.openai_api_key_secret_arn).await?);
    let authenticator = Arc::new(RequestAuthenticator::new(api_key_hasher, engineer_auth));
    let rate_limiter = Arc::new(RateLimiter::new(
        DynamoDbClient::new(&aws_config),
        config.rate_limit_table_name.clone(),
        config.rate_limit_requests_per_window,
        config.rate_limit_window,
    ));
    let token_usage_checker = Arc::new(TokenUsageChecker::new(
        DynamoDbClient::new(&aws_config),
        config.token_usage_table_name.clone(),
    ));
    let stream_tracker = Arc::new(ActiveStreamTracker::new(config.max_active_streams));
    start_active_stream_metric_publisher(
        Arc::clone(&stream_tracker),
        config.metric_interval,
        CloudWatchClient::new(&aws_config),
    );

    let address = SocketAddr::from(([0, 0, 0, 0], config.port));
    let listener = TcpListener::bind(address).await?;

    info!(%address, "proxy service listening");

    axum::serve(
        listener,
        app(AppState::new(
            anthropic_proxy,
            authenticator,
            openai_proxy,
            rate_limiter,
            stream_tracker,
            token_usage_checker,
        )),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    Ok(())
}
