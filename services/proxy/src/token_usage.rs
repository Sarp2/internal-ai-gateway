use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::{SystemTime, UNIX_EPOCH};

use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::{AttributeValue, Put, TransactWriteItem, Update};

use crate::engineer_auth::AuthenticatedEngineer;

const DAILY_WINDOW_PREFIX: &str = "daily";
const CONSUMED_TOKENS_ATTRIBUTE: &str = "consumed_tokens";
const TOKEN_COUNT_ATTRIBUTE: &str = "token_count";
const USAGE_WINDOW_ATTRIBUTE: &str = "usage_window";
const USER_ID_ATTRIBUTE: &str = "user_id";
const WEEKLY_WINDOW_PREFIX: &str = "weekly";
const DAILY_WINDOW_SECONDS: u64 = 86_400;
const WEEKLY_WINDOW_SECONDS: u64 = DAILY_WINDOW_SECONDS * 7;
const MONDAY_WEEK_OFFSET_SECONDS: u64 = DAILY_WINDOW_SECONDS * 3;
const RECONCILIATION_RECORD_PREFIX: &str = "reconciliation";

#[derive(Clone)]
pub struct TokenUsageChecker {
    dynamodb_client: DynamoDbClient,
    table_name: String,
}

impl TokenUsageChecker {
    pub fn new(dynamodb_client: DynamoDbClient, table_name: impl Into<String>) -> Self {
        Self {
            dynamodb_client,
            table_name: table_name.into(),
        }
    }

    pub async fn check_limits(
        &self,
        engineer: &AuthenticatedEngineer,
    ) -> Result<TokenUsageDecision, TokenUsageError> {
        if engineer.daily_token_limit.is_none() && engineer.weekly_token_limit.is_none() {
            return Ok(TokenUsageDecision {
                daily_tokens: 0,
                weekly_tokens: 0,
            });
        }

        let now = current_epoch_seconds()?;
        let daily_window = daily_usage_window(now);
        let weekly_window = weekly_usage_window(now);
        let daily_tokens = if engineer.daily_token_limit.is_some() {
            self.read_token_count(&engineer.user_id, &daily_window)
                .await?
        } else {
            0
        };
        let weekly_tokens = if engineer.weekly_token_limit.is_some() {
            self.read_token_count(&engineer.user_id, &weekly_window)
                .await?
        } else {
            0
        };

        if let Some(limit) = engineer.daily_token_limit
            && daily_tokens >= limit
        {
            return Err(TokenUsageError::DailyLimitExceeded);
        }

        if let Some(limit) = engineer.weekly_token_limit
            && weekly_tokens >= limit
        {
            return Err(TokenUsageError::WeeklyLimitExceeded);
        }

        Ok(TokenUsageDecision {
            daily_tokens,
            weekly_tokens,
        })
    }

    pub async fn record_input_tokens(
        &self,
        engineer: &AuthenticatedEngineer,
        input_tokens: u64,
    ) -> Result<(), TokenUsageError> {
        self.record_tokens_with_limit(engineer, input_tokens, true)
            .await
    }

    pub async fn record_tokens(
        &self,
        engineer: &AuthenticatedEngineer,
        token_count: u64,
    ) -> Result<(), TokenUsageError> {
        self.record_tokens_with_limit(engineer, token_count, false)
            .await
    }

    pub(crate) async fn record_reconciliation(
        &self,
        job_id: &str,
        user_id: &str,
        token_count: u64,
        occurred_at: u64,
    ) -> Result<(), TokenUsageError> {
        if token_count == 0 {
            return Ok(());
        }

        let daily_window_start = daily_usage_window_start(occurred_at);
        let weekly_window_start = weekly_usage_window_start(occurred_at);
        let reconciliation_window = format!("{RECONCILIATION_RECORD_PREFIX}#{job_id}");
        let ttl = token_usage_ttl_epoch_seconds(weekly_window_start, WEEKLY_WINDOW_SECONDS);

        let daily_update = self.usage_update(
            user_id,
            &daily_usage_window(occurred_at),
            token_count,
            None,
            token_usage_ttl_epoch_seconds(daily_window_start, DAILY_WINDOW_SECONDS),
            TokenUsageError::DailyLimitExceeded,
        )?;

        let weekly_update = self.usage_update(
            user_id,
            &weekly_usage_window(occurred_at),
            token_count,
            None,
            ttl,
            TokenUsageError::WeeklyLimitExceeded,
        )?;

        let marker = self.reconciliation_marker(user_id, &reconciliation_window, ttl)?;

        let result = self
            .dynamodb_client
            .transact_write_items()
            .client_request_token(job_id)
            .transact_items(TransactWriteItem::builder().update(daily_update).build())
            .transact_items(TransactWriteItem::builder().update(weekly_update).build())
            .transact_items(TransactWriteItem::builder().put(marker).build())
            .send()
            .await;

        match result {
            Ok(_) => Ok(()),
            Err(source)
                if source
                    .as_service_error()
                    .is_some_and(|error| error.is_transaction_canceled_exception())
                    && self
                        .reconciliation_marker_exists(user_id, &reconciliation_window)
                        .await? =>
            {
                Ok(())
            }
            Err(source) => Err(TokenUsageError::WriteFailed {
                source: Box::new(source),
            }),
        }
    }

    async fn record_tokens_with_limit(
        &self,
        engineer: &AuthenticatedEngineer,
        token_count: u64,
        enforce_limit: bool,
    ) -> Result<(), TokenUsageError> {
        if token_count == 0 {
            return Ok(());
        }

        let now = current_epoch_seconds()?;
        let daily_window_start = daily_usage_window_start(now);
        let weekly_window_start = weekly_usage_window_start(now);

        let daily_update = self.usage_update(
            &engineer.user_id,
            &daily_usage_window(now),
            token_count,
            enforce_limit
                .then_some(engineer.daily_token_limit)
                .flatten(),
            token_usage_ttl_epoch_seconds(daily_window_start, DAILY_WINDOW_SECONDS),
            TokenUsageError::DailyLimitExceeded,
        )?;

        let weekly_update = self.usage_update(
            &engineer.user_id,
            &weekly_usage_window(now),
            token_count,
            enforce_limit
                .then_some(engineer.weekly_token_limit)
                .flatten(),
            token_usage_ttl_epoch_seconds(weekly_window_start, WEEKLY_WINDOW_SECONDS),
            TokenUsageError::WeeklyLimitExceeded,
        )?;

        self.dynamodb_client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().update(daily_update).build())
            .transact_items(TransactWriteItem::builder().update(weekly_update).build())
            .send()
            .await
            .map_err(|source| {
                if enforce_limit
                    && source
                        .as_service_error()
                        .is_some_and(|error| error.is_transaction_canceled_exception())
                {
                    TokenUsageError::LimitExceededDuringInputRecording
                } else {
                    TokenUsageError::WriteFailed {
                        source: Box::new(source),
                    }
                }
            })?;

        Ok(())
    }

    async fn read_token_count(
        &self,
        user_id: &str,
        usage_window: &str,
    ) -> Result<u64, TokenUsageError> {
        let output = self
            .dynamodb_client
            .get_item()
            .table_name(&self.table_name)
            .key(USER_ID_ATTRIBUTE, AttributeValue::S(user_id.to_string()))
            .key(
                USAGE_WINDOW_ATTRIBUTE,
                AttributeValue::S(usage_window.to_string()),
            )
            .consistent_read(true)
            .projection_expression("#token_count")
            .expression_attribute_names("#token_count", TOKEN_COUNT_ATTRIBUTE)
            .send()
            .await
            .map_err(|source| TokenUsageError::ReadFailed {
                source: Box::new(source),
            })?;

        output
            .item()
            .and_then(|item| item.get(TOKEN_COUNT_ATTRIBUTE))
            .map(token_count_from_attribute)
            .transpose()
            .map(|tokens| tokens.unwrap_or(0))
    }

    async fn reconciliation_marker_exists(
        &self,
        user_id: &str,
        reconciliation_window: &str,
    ) -> Result<bool, TokenUsageError> {
        self.dynamodb_client
            .get_item()
            .table_name(&self.table_name)
            .key(USER_ID_ATTRIBUTE, AttributeValue::S(user_id.to_string()))
            .key(
                USAGE_WINDOW_ATTRIBUTE,
                AttributeValue::S(reconciliation_window.to_string()),
            )
            .consistent_read(true)
            .send()
            .await
            .map(|output| output.item.is_some())
            .map_err(|source| TokenUsageError::ReadFailed {
                source: Box::new(source),
            })
    }

    fn reconciliation_marker(
        &self,
        user_id: &str,
        reconciliation_window: &str,
        ttl: u64,
    ) -> Result<Put, TokenUsageError> {
        Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(HashMap::from([
                (
                    USER_ID_ATTRIBUTE.to_string(),
                    AttributeValue::S(user_id.to_string()),
                ),
                (
                    USAGE_WINDOW_ATTRIBUTE.to_string(),
                    AttributeValue::S(reconciliation_window.to_string()),
                ),
                (
                    "record_type".to_string(),
                    AttributeValue::S("usage_reconciliation".to_string()),
                ),
                ("ttl".to_string(), AttributeValue::N(ttl.to_string())),
            ])))
            .condition_expression("attribute_not_exists(#usage_window)")
            .expression_attribute_names("#usage_window", USAGE_WINDOW_ATTRIBUTE)
            .build()
            .map_err(|source| TokenUsageError::BuildWriteFailed {
                source: Box::new(source),
            })
    }

    fn usage_update(
        &self,
        user_id: &str,
        usage_window: &str,
        token_count: u64,
        token_limit: Option<u64>,
        ttl: u64,
        limit_error: TokenUsageError,
    ) -> Result<Update, TokenUsageError> {
        let mut update = Update::builder()
            .table_name(&self.table_name)
            .key(USER_ID_ATTRIBUTE, AttributeValue::S(user_id.to_string()))
            .key(
                USAGE_WINDOW_ATTRIBUTE,
                AttributeValue::S(usage_window.to_string()),
            )
            .update_expression("SET #ttl = :ttl ADD #token_count :tokens, #consumed_tokens :tokens")
            .expression_attribute_names("#consumed_tokens", CONSUMED_TOKENS_ATTRIBUTE)
            .expression_attribute_names("#token_count", TOKEN_COUNT_ATTRIBUTE)
            .expression_attribute_names("#ttl", "ttl")
            .expression_attribute_values(":tokens", AttributeValue::N(token_count.to_string()))
            .expression_attribute_values(":ttl", AttributeValue::N(ttl.to_string()));

        if let Some(limit) = token_limit {
            let remaining_before_increment = limit.checked_sub(token_count).ok_or(limit_error)?;
            update = update
				.condition_expression(
					"attribute_not_exists(#token_count) OR #token_count <= :remaining_before_increment",
				)
				.expression_attribute_values(
					":remaining_before_increment",
					AttributeValue::N(remaining_before_increment.to_string()),
				);
        }

        update
            .build()
            .map_err(|source| TokenUsageError::BuildWriteFailed {
                source: Box::new(source),
            })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct TokenUsageDecision {
    pub daily_tokens: u64,
    pub weekly_tokens: u64,
}

pub(crate) fn daily_usage_window(epoch_seconds: u64) -> String {
    format!(
        "{DAILY_WINDOW_PREFIX}#{}",
        epoch_seconds / DAILY_WINDOW_SECONDS
    )
}

pub(crate) fn weekly_usage_window(epoch_seconds: u64) -> String {
    format!(
        "{WEEKLY_WINDOW_PREFIX}#{}",
        epoch_seconds.saturating_add(MONDAY_WEEK_OFFSET_SECONDS) / WEEKLY_WINDOW_SECONDS
    )
}

pub(crate) fn daily_usage_window_start(epoch_seconds: u64) -> u64 {
    epoch_seconds - (epoch_seconds % DAILY_WINDOW_SECONDS)
}

pub(crate) fn weekly_usage_window_start(epoch_seconds: u64) -> u64 {
    let monday_aligned_seconds = epoch_seconds.saturating_add(MONDAY_WEEK_OFFSET_SECONDS);
    let monday_aligned_start =
        monday_aligned_seconds - (monday_aligned_seconds % WEEKLY_WINDOW_SECONDS);

    monday_aligned_start.saturating_sub(MONDAY_WEEK_OFFSET_SECONDS)
}

pub(crate) fn token_usage_ttl_epoch_seconds(window_start: u64, window_seconds: u64) -> u64 {
    window_start.saturating_add(window_seconds.saturating_mul(2))
}

pub(crate) fn token_count_from_attribute(
    attribute: &AttributeValue,
) -> Result<u64, TokenUsageError> {
    match attribute {
        AttributeValue::N(value) => value
            .parse::<u64>()
            .map_err(|_| TokenUsageError::InvalidUsageItem),
        _ => Err(TokenUsageError::InvalidUsageItem),
    }
}

pub(crate) fn current_epoch_seconds() -> Result<u64, TokenUsageError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|source| TokenUsageError::ClockFailed {
            source: Box::new(source),
        })
}

#[derive(Debug)]
pub enum TokenUsageError {
    DailyLimitExceeded,
    WeeklyLimitExceeded,
    LimitExceededDuringInputRecording,
    ReadFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    WriteFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    BuildWriteFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    InvalidUsageItem,
    ClockFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
}

impl Display for TokenUsageError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DailyLimitExceeded => write!(formatter, "daily token limit exceeded"),
            Self::WeeklyLimitExceeded => write!(formatter, "weekly token limit exceeded"),
            Self::LimitExceededDuringInputRecording => {
                write!(
                    formatter,
                    "token limit exceeded while recording input tokens"
                )
            }
            Self::ReadFailed { source } => {
                write!(formatter, "failed to read token usage: {source}")
            }
            Self::WriteFailed { source } => {
                write!(formatter, "failed to write token usage: {source}")
            }
            Self::BuildWriteFailed { source } => {
                write!(formatter, "failed to build token usage write: {source}")
            }
            Self::InvalidUsageItem => write!(formatter, "token usage item is invalid"),
            Self::ClockFailed { source } => {
                write!(formatter, "failed to read system clock: {source}")
            }
        }
    }
}

impl Error for TokenUsageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::ReadFailed { source }
            | Self::WriteFailed { source }
            | Self::BuildWriteFailed { source }
            | Self::ClockFailed { source } => Some(source.as_ref()),
            Self::DailyLimitExceeded
            | Self::WeeklyLimitExceeded
            | Self::LimitExceededDuringInputRecording
            | Self::InvalidUsageItem => None,
        }
    }
}
