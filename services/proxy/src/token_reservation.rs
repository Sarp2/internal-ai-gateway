use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError;
use aws_sdk_dynamodb::types::{AttributeValue, CancellationReason, Put, TransactWriteItem, Update};
use tokio::time::sleep;
use uuid::Uuid;

use crate::engineer_auth::AuthenticatedEngineer;
use crate::token_usage::{
    TokenUsageChecker, TokenUsageError, current_epoch_seconds, daily_usage_window,
    daily_usage_window_start, token_usage_ttl_epoch_seconds, weekly_usage_window,
    weekly_usage_window_start,
};

const CONSUMED_TOKENS_ATTRIBUTE: &str = "consumed_tokens";
const DAILY_WINDOW_SECONDS: u64 = 86_400;
const RECORD_TYPE_ATTRIBUTE: &str = "record_type";
const RESERVATION_RETRY_BASE_DELAY_MILLISECONDS: u64 = 25;
const RESERVATION_TRANSACTION_ATTEMPTS: usize = 4;
const REQUEST_ID_ATTRIBUTE: &str = "request_id";
const RESERVED_TOKENS_ATTRIBUTE: &str = "reserved_tokens";
const RESERVATION_RECORD_TYPE: &str = "token_reservation";
const RESERVATION_STATUS_ATTRIBUTE: &str = "status";
const RESERVATION_STATUS_COMPLETED: &str = "completed";
const RESERVATION_STATUS_RESERVED: &str = "reserved";
const TOKEN_COUNT_ATTRIBUTE: &str = "token_count";
const USAGE_WINDOW_ATTRIBUTE: &str = "usage_window";
const USER_ID_ATTRIBUTE: &str = "user_id";
const WEEKLY_WINDOW_SECONDS: u64 = DAILY_WINDOW_SECONDS * 7;

#[derive(Clone)]
pub struct TokenReservationManager {
    dynamodb_client: DynamoDbClient,
    table_name: String,
    token_usage_checker: Arc<TokenUsageChecker>,
}

impl TokenReservationManager {
    pub fn new(
        dynamodb_client: DynamoDbClient,
        table_name: impl Into<String>,
        token_usage_checker: Arc<TokenUsageChecker>,
    ) -> Self {
        Self {
            dynamodb_client,
            table_name: table_name.into(),
            token_usage_checker,
        }
    }

    pub async fn reserve(
        self: &Arc<Self>,
        engineer: AuthenticatedEngineer,
        token_budget: u64,
    ) -> Result<TokenReservation, TokenReservationError> {
        if token_budget == 0 {
            return Err(TokenReservationError::InvalidBudget);
        }

        if engineer.daily_token_limit.is_none() && engineer.weekly_token_limit.is_none() {
            return Ok(TokenReservation::untracked(
                Arc::clone(self),
                engineer,
                token_budget,
            ));
        }

        let now = current_epoch_seconds().map_err(TokenReservationError::Usage)?;
        let daily_window = daily_usage_window(now);
        let weekly_window = weekly_usage_window(now);
        let reservation_id = Uuid::new_v4().to_string();
        let completion_token = Uuid::new_v4().to_string();
        let reservation_window = format!("reservation#{reservation_id}");
        let ttl =
            token_usage_ttl_epoch_seconds(weekly_usage_window_start(now), WEEKLY_WINDOW_SECONDS);

        let daily_update = self.reservation_update(
            &engineer.user_id,
            &daily_window,
            token_budget,
            engineer.daily_token_limit,
            token_usage_ttl_epoch_seconds(daily_usage_window_start(now), DAILY_WINDOW_SECONDS),
        )?;
        let weekly_update = self.reservation_update(
            &engineer.user_id,
            &weekly_window,
            token_budget,
            engineer.weekly_token_limit,
            ttl,
        )?;
        let reservation_put = self.reservation_put(ReservationRecord {
            daily_window: &daily_window,
            request_id: &reservation_id,
            reservation_window: &reservation_window,
            token_budget,
            ttl,
            user_id: &engineer.user_id,
            weekly_window: &weekly_window,
        })?;

        for attempt in 0..RESERVATION_TRANSACTION_ATTEMPTS {
            let result = self
                .dynamodb_client
                .transact_write_items()
                .client_request_token(&reservation_id)
                .transact_items(
                    TransactWriteItem::builder()
                        .update(daily_update.clone())
                        .build(),
                )
                .transact_items(
                    TransactWriteItem::builder()
                        .update(weekly_update.clone())
                        .build(),
                )
                .transact_items(
                    TransactWriteItem::builder()
                        .put(reservation_put.clone())
                        .build(),
                )
                .send()
                .await;

            match result {
                Ok(_) => break,
                Err(source) => {
                    let cancellation = source
                        .as_service_error()
                        .map(classify_reservation_error)
                        .unwrap_or(ReservationFailure::WriteFailed);

                    match cancellation {
                        ReservationFailure::LimitExceeded => {
                            return Err(TokenReservationError::LimitExceeded);
                        }
                        ReservationFailure::Retry
                            if attempt + 1 < RESERVATION_TRANSACTION_ATTEMPTS =>
                        {
                            sleep(reservation_retry_delay(attempt)).await;
                        }
                        ReservationFailure::Retry | ReservationFailure::WriteFailed => {
                            return Err(TokenReservationError::WriteFailed {
                                source: Box::new(source),
                            });
                        }
                    }
                }
            }
        }

        Ok(TokenReservation {
            completion_token,
            daily_window,
            engineer,
            manager: Arc::clone(self),
            reservation_window: Some(reservation_window),
            token_budget,
            weekly_window,
        })
    }

    fn reservation_update(
        &self,
        user_id: &str,
        usage_window: &str,
        token_budget: u64,
        token_limit: Option<u64>,
        ttl: u64,
    ) -> Result<Update, TokenReservationError> {
        // token_count remains the effective total: consumed tokens plus active reservations.
        let mut update = Update::builder()
            .table_name(&self.table_name)
            .key(USER_ID_ATTRIBUTE, AttributeValue::S(user_id.to_string()))
            .key(
                USAGE_WINDOW_ATTRIBUTE,
                AttributeValue::S(usage_window.to_string()),
            )
            .update_expression("SET #ttl = :ttl ADD #token_count :budget, #reserved_tokens :budget")
            .expression_attribute_names("#token_count", TOKEN_COUNT_ATTRIBUTE)
            .expression_attribute_names("#reserved_tokens", RESERVED_TOKENS_ATTRIBUTE)
            .expression_attribute_names("#ttl", "ttl")
            .expression_attribute_values(":budget", AttributeValue::N(token_budget.to_string()))
            .expression_attribute_values(":ttl", AttributeValue::N(ttl.to_string()));

        if let Some(limit) = token_limit {
            let remaining_before_reservation = limit
                .checked_sub(token_budget)
                .ok_or(TokenReservationError::LimitExceeded)?;
            update = update
                .condition_expression(
                    "attribute_not_exists(#token_count) OR #token_count <= :remaining",
                )
                .expression_attribute_values(
                    ":remaining",
                    AttributeValue::N(remaining_before_reservation.to_string()),
                );
        }

        update
            .build()
            .map_err(|source| TokenReservationError::BuildWriteFailed {
                source: Box::new(source),
            })
    }

    fn reservation_put(&self, record: ReservationRecord<'_>) -> Result<Put, TokenReservationError> {
        let item = HashMap::from([
            (
                USER_ID_ATTRIBUTE.to_string(),
                AttributeValue::S(record.user_id.to_string()),
            ),
            (
                USAGE_WINDOW_ATTRIBUTE.to_string(),
                AttributeValue::S(record.reservation_window.to_string()),
            ),
            (
                RECORD_TYPE_ATTRIBUTE.to_string(),
                AttributeValue::S(RESERVATION_RECORD_TYPE.to_string()),
            ),
            (
                REQUEST_ID_ATTRIBUTE.to_string(),
                AttributeValue::S(record.request_id.to_string()),
            ),
            (
                RESERVATION_STATUS_ATTRIBUTE.to_string(),
                AttributeValue::S(RESERVATION_STATUS_RESERVED.to_string()),
            ),
            (
                RESERVED_TOKENS_ATTRIBUTE.to_string(),
                AttributeValue::N(record.token_budget.to_string()),
            ),
            (
                "daily_window".to_string(),
                AttributeValue::S(record.daily_window.to_string()),
            ),
            (
                "weekly_window".to_string(),
                AttributeValue::S(record.weekly_window.to_string()),
            ),
            ("ttl".to_string(), AttributeValue::N(record.ttl.to_string())),
            (
                "expires_at".to_string(),
                AttributeValue::N(record.ttl.to_string()),
            ),
        ]);

        Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(item))
            .condition_expression("attribute_not_exists(#usage_window)")
            .expression_attribute_names("#usage_window", USAGE_WINDOW_ATTRIBUTE)
            .build()
            .map_err(|source| TokenReservationError::BuildWriteFailed {
                source: Box::new(source),
            })
    }

    async fn reconcile(
        &self,
        reservation: &TokenReservation,
        actual_tokens: Option<u64>,
    ) -> Result<(), TokenReservationError> {
        let (token_count_adjustment, reserved_adjustment, charged_tokens) =
            reconciliation_values(reservation.token_budget, actual_tokens);

        let Some(reservation_window) = reservation.reservation_window.as_deref() else {
            let Some(actual_tokens) = actual_tokens else {
                return Ok(());
            };

            return self
                .token_usage_checker
                .record_tokens(&reservation.engineer, actual_tokens)
                .await
                .map_err(TokenReservationError::Usage);
        };

        let daily_update = self.reconciliation_update(
            &reservation.engineer.user_id,
            &reservation.daily_window,
            token_count_adjustment,
            reserved_adjustment,
            charged_tokens,
        )?;
        let weekly_update = self.reconciliation_update(
            &reservation.engineer.user_id,
            &reservation.weekly_window,
            token_count_adjustment,
            reserved_adjustment,
            charged_tokens,
        )?;
        let reservation_update = Update::builder()
            .table_name(&self.table_name)
            .key(
                USER_ID_ATTRIBUTE,
                AttributeValue::S(reservation.engineer.user_id.clone()),
            )
            .key(
                USAGE_WINDOW_ATTRIBUTE,
                AttributeValue::S(reservation_window.to_string()),
            )
            .update_expression(
                "SET #reservation_status = :completed, #actual_tokens = :actual_tokens",
            )
            .condition_expression("#reservation_status = :reserved")
            .expression_attribute_names("#actual_tokens", "actual_tokens")
            .expression_attribute_names("#reservation_status", RESERVATION_STATUS_ATTRIBUTE)
            .expression_attribute_values(
                ":actual_tokens",
                AttributeValue::N(charged_tokens.to_string()),
            )
            .expression_attribute_values(
                ":completed",
                AttributeValue::S(RESERVATION_STATUS_COMPLETED.to_string()),
            )
            .expression_attribute_values(
                ":reserved",
                AttributeValue::S(RESERVATION_STATUS_RESERVED.to_string()),
            )
            .build()
            .map_err(|source| TokenReservationError::BuildWriteFailed {
                source: Box::new(source),
            })?;

        self.dynamodb_client
            .transact_write_items()
            .client_request_token(&reservation.completion_token)
            .transact_items(TransactWriteItem::builder().update(daily_update).build())
            .transact_items(TransactWriteItem::builder().update(weekly_update).build())
            .transact_items(
                TransactWriteItem::builder()
                    .update(reservation_update)
                    .build(),
            )
            .send()
            .await
            .map_err(|source| TokenReservationError::WriteFailed {
                source: Box::new(source),
            })?;

        Ok(())
    }

    fn reconciliation_update(
        &self,
        user_id: &str,
        usage_window: &str,
        token_count_adjustment: i128,
        reserved_adjustment: i128,
        consumed_tokens: u64,
    ) -> Result<Update, TokenReservationError> {
        Update::builder()
            .table_name(&self.table_name)
            .key(USER_ID_ATTRIBUTE, AttributeValue::S(user_id.to_string()))
            .key(
                USAGE_WINDOW_ATTRIBUTE,
                AttributeValue::S(usage_window.to_string()),
            )
            .update_expression(
                "ADD #token_count :token_adjustment, #reserved_tokens :reserved_adjustment, #consumed_tokens :consumed_tokens",
            )
            .expression_attribute_names("#consumed_tokens", CONSUMED_TOKENS_ATTRIBUTE)
            .expression_attribute_names("#reserved_tokens", RESERVED_TOKENS_ATTRIBUTE)
            .expression_attribute_names("#token_count", TOKEN_COUNT_ATTRIBUTE)
            .expression_attribute_values(
                ":consumed_tokens",
                AttributeValue::N(consumed_tokens.to_string()),
            )
            .expression_attribute_values(
                ":reserved_adjustment",
                AttributeValue::N(reserved_adjustment.to_string()),
            )
            .expression_attribute_values(
                ":token_adjustment",
                AttributeValue::N(token_count_adjustment.to_string()),
            )
            .build()
            .map_err(|source| TokenReservationError::BuildWriteFailed {
                source: Box::new(source),
            })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReservationFailure {
    LimitExceeded,
    Retry,
    WriteFailed,
}

fn classify_reservation_error(error: &TransactWriteItemsError) -> ReservationFailure {
    let TransactWriteItemsError::TransactionCanceledException(error) = error else {
        return ReservationFailure::WriteFailed;
    };

    classify_cancellation_reasons(error.cancellation_reasons())
}

pub(crate) fn classify_cancellation_reasons(reasons: &[CancellationReason]) -> ReservationFailure {
    if reasons
        .iter()
        .take(2)
        .any(|reason| reason.code() == Some("ConditionalCheckFailed"))
    {
        return ReservationFailure::LimitExceeded;
    }

    if reasons.iter().any(|reason| {
        matches!(
            reason.code(),
            Some("TransactionConflict" | "ProvisionedThroughputExceeded" | "ThrottlingError")
        )
    }) {
        return ReservationFailure::Retry;
    }

    ReservationFailure::WriteFailed
}

fn reservation_retry_delay(attempt: usize) -> Duration {
    let base_delay = RESERVATION_RETRY_BASE_DELAY_MILLISECONDS * (1_u64 << attempt);
    let jitter = fastrand::u64(0..=base_delay);

    Duration::from_millis(base_delay + jitter)
}

struct ReservationRecord<'a> {
    daily_window: &'a str,
    request_id: &'a str,
    reservation_window: &'a str,
    token_budget: u64,
    ttl: u64,
    user_id: &'a str,
    weekly_window: &'a str,
}

pub(crate) fn reconciliation_values(
    token_budget: u64,
    actual_tokens: Option<u64>,
) -> (i128, i128, u64) {
    let charged_tokens = actual_tokens.unwrap_or(token_budget);
    (
        i128::from(charged_tokens) - i128::from(token_budget),
        -i128::from(token_budget),
        charged_tokens,
    )
}

pub struct TokenReservation {
    completion_token: String,
    daily_window: String,
    engineer: AuthenticatedEngineer,
    manager: Arc<TokenReservationManager>,
    reservation_window: Option<String>,
    token_budget: u64,
    weekly_window: String,
}

impl TokenReservation {
    fn untracked(
        manager: Arc<TokenReservationManager>,
        engineer: AuthenticatedEngineer,
        token_budget: u64,
    ) -> Self {
        Self {
            completion_token: String::new(),
            daily_window: String::new(),
            engineer,
            manager,
            reservation_window: None,
            token_budget,
            weekly_window: String::new(),
        }
    }

    pub async fn reconcile(self, actual_tokens: Option<u64>) -> Result<(), TokenReservationError> {
        self.manager.reconcile(&self, actual_tokens).await
    }

    pub fn engineer_id(&self) -> &str {
        &self.engineer.user_id
    }
}

#[derive(Debug)]
pub enum TokenReservationError {
    InvalidBudget,
    LimitExceeded,
    Usage(TokenUsageError),
    WriteFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    BuildWriteFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
}

impl Display for TokenReservationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBudget => write!(formatter, "token reservation budget must be positive"),
            Self::LimitExceeded => write!(formatter, "token limit exceeded"),
            Self::Usage(error) => write!(formatter, "token usage operation failed: {error}"),
            Self::WriteFailed { source } => {
                write!(formatter, "failed to write token reservation: {source}")
            }
            Self::BuildWriteFailed { source } => {
                write!(
                    formatter,
                    "failed to build token reservation write: {source}"
                )
            }
        }
    }
}

impl Error for TokenReservationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Usage(error) => Some(error),
            Self::WriteFailed { source } | Self::BuildWriteFailed { source } => {
                Some(source.as_ref())
            }
            Self::InvalidBudget | Self::LimitExceeded => None,
        }
    }
}
