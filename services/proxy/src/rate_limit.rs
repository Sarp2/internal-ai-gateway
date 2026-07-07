use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::AttributeValue;

const REQUEST_COUNT_ATTRIBUTE: &str = "request_count";
const REQUEST_TS_ATTRIBUTE: &str = "request_ts";
const TTL_ATTRIBUTE: &str = "ttl";
const USER_ID_ATTRIBUTE: &str = "user_id";

#[derive(Clone)]
pub struct RateLimiter {
    dynamodb_client: DynamoDbClient,
    requests_per_window: u64,
    table_name: String,
    window: Duration,
}

impl RateLimiter {
    pub fn new(
        dynamodb_client: DynamoDbClient,
        table_name: impl Into<String>,
        requests_per_window: u64,
        window: Duration,
    ) -> Self {
        Self {
            dynamodb_client,
            requests_per_window: requests_per_window.max(1),
            table_name: table_name.into(),
            window: window.max(Duration::from_secs(1)),
        }
    }

    pub async fn check_and_record(
        &self,
        user_id: &str,
    ) -> Result<RateLimitDecision, RateLimitError> {
        let now = current_epoch_seconds()?;
        let window_start = window_start_epoch_seconds(now, self.window);
        let ttl = rate_limit_ttl_epoch_seconds(window_start, self.window);

        self.dynamodb_client
            .update_item()
            .table_name(&self.table_name)
            .key(USER_ID_ATTRIBUTE, AttributeValue::S(user_id.to_string()))
            .key(
                REQUEST_TS_ATTRIBUTE,
                AttributeValue::N(window_start.to_string()),
            )
            .update_expression("SET #ttl = :ttl ADD #request_count :one")
            .condition_expression(
                "attribute_not_exists(#request_count) OR #request_count < :request_limit",
            )
            .expression_attribute_names("#request_count", REQUEST_COUNT_ATTRIBUTE)
            .expression_attribute_names("#ttl", TTL_ATTRIBUTE)
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .expression_attribute_values(
                ":request_limit",
                AttributeValue::N(self.requests_per_window.to_string()),
            )
            .expression_attribute_values(":ttl", AttributeValue::N(ttl.to_string()))
            .send()
            .await
            .map_err(|source| {
                if source
                    .as_service_error()
                    .is_some_and(|error| error.is_conditional_check_failed_exception())
                {
                    RateLimitError::RateLimitExceeded
                } else {
                    RateLimitError::UpdateFailed {
                        source: Box::new(source),
                    }
                }
            })?;

        Ok(RateLimitDecision {
            limit: self.requests_per_window,
            window_start_epoch_seconds: window_start,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct RateLimitDecision {
    pub limit: u64,
    pub window_start_epoch_seconds: u64,
}

pub(crate) fn window_start_epoch_seconds(now_epoch_seconds: u64, window: Duration) -> u64 {
    let window_seconds = window.as_secs().max(1);

    now_epoch_seconds - (now_epoch_seconds % window_seconds)
}

pub(crate) fn rate_limit_ttl_epoch_seconds(window_start: u64, window: Duration) -> u64 {
    window_start.saturating_add(window.as_secs().max(1).saturating_mul(2))
}

fn current_epoch_seconds() -> Result<u64, RateLimitError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|source| RateLimitError::ClockFailed {
            source: Box::new(source),
        })
}

#[derive(Debug)]
pub enum RateLimitError {
    RateLimitExceeded,
    UpdateFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    ClockFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
}

impl Display for RateLimitError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimitExceeded => write!(formatter, "rate limit exceeded"),
            Self::UpdateFailed { source } => {
                write!(formatter, "failed to update rate limit: {source}")
            }
            Self::ClockFailed { source } => {
                write!(formatter, "failed to read system clock: {source}")
            }
        }
    }
}

impl Error for RateLimitError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RateLimitExceeded => None,
            Self::UpdateFailed { source } | Self::ClockFailed { source } => Some(source.as_ref()),
        }
    }
}
