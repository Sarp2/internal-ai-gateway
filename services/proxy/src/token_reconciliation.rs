use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_sqs::Client as SqsClient;
use aws_sdk_sqs::types::Message;
use serde::{Deserialize, Serialize};
use tokio::time::sleep;
use tracing::{error, warn};

use crate::background_tasks::BackgroundTasks;
use crate::token_reservation::TokenReservationManager;

const RECEIVE_BATCH_SIZE: i32 = 10;
const RECEIVE_ERROR_DELAY: Duration = Duration::from_secs(1);
const RECEIVE_WAIT_SECONDS: i32 = 20;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum TokenReconciliationJob {
    Reservation {
        actual_tokens: Option<u64>,
        completion_token: String,
        daily_window: String,
        reservation_window: String,
        token_budget: u64,
        user_id: String,
        weekly_window: String,
    },
    Usage {
        job_id: String,
        occurred_at: u64,
        token_count: u64,
        user_id: String,
    },
}

#[derive(Clone)]
pub(crate) struct TokenReconciliationQueue {
    client: SqsClient,
    queue_url: Arc<str>,
}

impl TokenReconciliationQueue {
    pub(crate) fn new(client: SqsClient, queue_url: impl Into<Arc<str>>) -> Self {
        Self {
            client,
            queue_url: queue_url.into(),
        }
    }

    pub(crate) async fn enqueue(
        &self,
        job: &TokenReconciliationJob,
    ) -> Result<(), TokenReconciliationQueueError> {
        let body = serde_json::to_string(job).map_err(TokenReconciliationQueueError::Serialize)?;

        self.client
            .send_message()
            .queue_url(self.queue_url.as_ref())
            .message_body(body)
            .send()
            .await
            .map_err(|source| TokenReconciliationQueueError::Send {
                source: Box::new(source),
            })?;

        Ok(())
    }

    async fn receive(&self) -> Result<Vec<Message>, TokenReconciliationQueueError> {
        self.client
            .receive_message()
            .queue_url(self.queue_url.as_ref())
            .max_number_of_messages(RECEIVE_BATCH_SIZE)
            .wait_time_seconds(RECEIVE_WAIT_SECONDS)
            .send()
            .await
            .map(|output| output.messages.unwrap_or_default())
            .map_err(|source| TokenReconciliationQueueError::Receive {
                source: Box::new(source),
            })
    }

    async fn delete(&self, receipt_handle: &str) -> Result<(), TokenReconciliationQueueError> {
        self.client
            .delete_message()
            .queue_url(self.queue_url.as_ref())
            .receipt_handle(receipt_handle)
            .send()
            .await
            .map_err(|source| TokenReconciliationQueueError::Delete {
                source: Box::new(source),
            })?;

        Ok(())
    }
}

pub(crate) fn start_reconciliation_worker(
    background_tasks: &BackgroundTasks,
    queue: TokenReconciliationQueue,
    reservation_manager: Arc<TokenReservationManager>,
) {
    let cancellation = background_tasks.cancellation_token();

    background_tasks.spawn(async move {
        loop {
            let messages = tokio::select! {
                () = cancellation.cancelled() => break,
                result = queue.receive() => match result {
                    Ok(messages) => messages,
                    Err(error) => {
                        error!(%error, "failed to receive token reconciliation jobs");
                        tokio::select! {
                            () = cancellation.cancelled() => break,
                            () = sleep(RECEIVE_ERROR_DELAY) => continue,
                        }
                    }
                },
            };

            for message in messages {
                if cancellation.is_cancelled() {
                    break;
                }

                process_message(&queue, &reservation_manager, message).await;
            }
        }
    });
}

async fn process_message(
    queue: &TokenReconciliationQueue,
    reservation_manager: &TokenReservationManager,
    message: Message,
) {
    let Some(body) = message.body() else {
        warn!(
            message_id = message.message_id(),
            "token reconciliation job has no body"
        );
        return;
    };

    let job = match serde_json::from_str::<TokenReconciliationJob>(body) {
        Ok(job) => job,
        Err(error) => {
            warn!(message_id = message.message_id(), %error, "invalid token reconciliation job");
            return;
        }
    };

    if let Err(error) = reservation_manager.process_reconciliation(&job).await {
        warn!(message_id = message.message_id(), %error, "token reconciliation job failed");
        return;
    }

    let Some(receipt_handle) = message.receipt_handle() else {
        warn!(
            message_id = message.message_id(),
            "token reconciliation job has no receipt handle"
        );
        return;
    };

    if let Err(error) = queue.delete(receipt_handle).await {
        warn!(message_id = message.message_id(), %error, "failed to delete completed token reconciliation job");
    }
}

#[derive(Debug)]
pub(crate) enum TokenReconciliationQueueError {
    Serialize(serde_json::Error),
    Send {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    Receive {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    Delete {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
}

impl Display for TokenReconciliationQueueError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(error) => {
                write!(
                    formatter,
                    "failed to serialize token reconciliation job: {error}"
                )
            }
            Self::Send { source } => {
                write!(
                    formatter,
                    "failed to enqueue token reconciliation job: {source}"
                )
            }
            Self::Receive { source } => {
                write!(
                    formatter,
                    "failed to receive token reconciliation jobs: {source}"
                )
            }
            Self::Delete { source } => {
                write!(
                    formatter,
                    "failed to delete token reconciliation job: {source}"
                )
            }
        }
    }
}

impl Error for TokenReconciliationQueueError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialize(error) => Some(error),
            Self::Send { source } | Self::Receive { source } | Self::Delete { source } => {
                Some(source.as_ref())
            }
        }
    }
}
