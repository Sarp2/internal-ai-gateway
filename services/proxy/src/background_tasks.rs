use std::future::Future;

use tokio::time::{Duration, timeout};
use tokio_util::task::TaskTracker;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Default)]
pub struct BackgroundTasks {
    tracker: TaskTracker,
}

impl BackgroundTasks {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn spawn<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.tracker.spawn(task);
    }

    pub async fn shutdown(&self) {
        self.tracker.close();

        if timeout(SHUTDOWN_TIMEOUT, self.tracker.wait())
            .await
            .is_err()
        {
            tracing::warn!("timed out waiting for background proxy tasks during shutdown");
        }
    }
}
