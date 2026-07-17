use std::sync::Arc;

use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_sqs::Client as SqsClient;

use crate::background_tasks::BackgroundTasks;
use crate::token_reconciliation::{TokenReconciliationQueue, start_reconciliation_worker};
use crate::token_reservation::TokenReservationManager;
use crate::token_usage::TokenUsageChecker;

#[derive(Clone)]
pub struct TokenAccounting {
    reservation_manager: Arc<TokenReservationManager>,
    usage_checker: Arc<TokenUsageChecker>,
}

impl TokenAccounting {
    pub fn new(
        dynamodb_client: DynamoDbClient,
        sqs_client: SqsClient,
        reconciliation_queue_url: impl Into<Arc<str>>,
        table_name: impl Into<String>,
    ) -> Self {
        let table_name = table_name.into();
        let reconciliation_queue =
            TokenReconciliationQueue::new(sqs_client, reconciliation_queue_url);
        let usage_checker = Arc::new(TokenUsageChecker::new(
            dynamodb_client.clone(),
            table_name.clone(),
        ));
        let reservation_manager = Arc::new(TokenReservationManager::new(
            dynamodb_client,
            table_name,
            reconciliation_queue,
            Arc::clone(&usage_checker),
        ));

        Self {
            reservation_manager,
            usage_checker,
        }
    }

    pub fn reservation_manager(&self) -> Arc<TokenReservationManager> {
        Arc::clone(&self.reservation_manager)
    }

    pub fn usage_checker(&self) -> Arc<TokenUsageChecker> {
        Arc::clone(&self.usage_checker)
    }

    pub fn start_reconciliation_worker(&self, background_tasks: &BackgroundTasks) {
        start_reconciliation_worker(
            background_tasks,
            self.reservation_manager.reconciliation_queue(),
            Arc::clone(&self.reservation_manager),
        );
    }
}
