use std::sync::Arc;
use std::time::Duration;

use aws_sdk_cloudwatch::Client as CloudWatchClient;
use aws_sdk_cloudwatch::types::{Dimension, MetricDatum, StandardUnit};
use tokio::time::MissedTickBehavior;

use crate::streams::ActiveStreamTracker;

const METRIC_NAMESPACE: &str = "InternalAiGateway/Proxy";
const ACTIVE_STREAMS_METRIC_NAME: &str = "ActiveStreams";
const SERVICE_NAME: &str = "internal-ai-gateway-proxy";

pub fn start_active_stream_metric_publisher(
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
