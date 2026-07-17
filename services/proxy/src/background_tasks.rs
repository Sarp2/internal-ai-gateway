use std::future::Future;

use tokio::time::{Duration, timeout};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Default)]
pub struct BackgroundTasks {
    cancellation: CancellationToken,
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

    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub async fn shutdown(&self) {
        self.cancel();
        self.tracker.close();

        if timeout(SHUTDOWN_TIMEOUT, self.tracker.wait())
            .await
            .is_err()
        {
            tracing::warn!("timed out waiting for background proxy tasks during shutdown");
        }
    }
}
