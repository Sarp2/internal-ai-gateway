use std::sync::Arc;

use aws_sdk_dynamodb::Client as DynamoDbClient;

use crate::token_reservation::TokenReservationManager;
use crate::token_usage::TokenUsageChecker;

#[derive(Clone)]
pub struct TokenAccounting {
    reservation_manager: Arc<TokenReservationManager>,
    usage_checker: Arc<TokenUsageChecker>,
}

impl TokenAccounting {
    pub fn new(dynamodb_client: DynamoDbClient, table_name: impl Into<String>) -> Self {
        let table_name = table_name.into();
        let usage_checker = Arc::new(TokenUsageChecker::new(
            dynamodb_client.clone(),
            table_name.clone(),
        ));
        let reservation_manager = Arc::new(TokenReservationManager::new(
            dynamodb_client,
            table_name,
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
}
