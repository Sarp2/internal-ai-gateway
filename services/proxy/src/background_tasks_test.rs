use std::time::Duration;

use crate::background_tasks::BackgroundTasks;

#[tokio::test]
async fn cancellation_can_start_before_waiting_for_tasks() {
    let background_tasks = BackgroundTasks::new();
    let cancellation = background_tasks.cancellation_token();

    background_tasks.cancel();

    tokio::time::timeout(Duration::from_millis(100), cancellation.cancelled())
        .await
        .expect("background task cancellation should be observable immediately");
    background_tasks.shutdown().await;
}
