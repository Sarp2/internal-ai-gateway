use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

use aws_sdk_dynamodb::Client as DynamoDbClient;
use aws_sdk_dynamodb::types::AttributeValue;

const API_KEY_HASH_ATTRIBUTE: &str = "api_key_hash";
const DAILY_TOKEN_LIMIT_ATTRIBUTE: &str = "daily_token_limit";
const ENABLED_ATTRIBUTE: &str = "enabled";
const USER_ID_ATTRIBUTE: &str = "user_id";
const WEEKLY_TOKEN_LIMIT_ATTRIBUTE: &str = "weekly_token_limit";

#[derive(Clone)]
pub struct EngineerAuth {
    dynamodb_client: DynamoDbClient,
    engineers_table_name: String,
    engineers_api_key_index_name: String,
}

impl EngineerAuth {
    pub fn new(
        dynamodb_client: DynamoDbClient,
        engineers_table_name: impl Into<String>,
        engineers_api_key_index_name: impl Into<String>,
    ) -> Self {
        Self {
            dynamodb_client,
            engineers_table_name: engineers_table_name.into(),
            engineers_api_key_index_name: engineers_api_key_index_name.into(),
        }
    }

    pub async fn find_engineer_by_api_key_hash(
        &self,
        api_key_hash: &str,
    ) -> Result<Option<AuthenticatedEngineer>, EngineerAuthError> {
        let output = self
            .dynamodb_client
            .query()
            .table_name(&self.engineers_table_name)
            .index_name(&self.engineers_api_key_index_name)
            .key_condition_expression("#api_key_hash = :api_key_hash")
            .expression_attribute_names("#api_key_hash", API_KEY_HASH_ATTRIBUTE)
            .expression_attribute_values(
                ":api_key_hash",
                AttributeValue::S(api_key_hash.to_string()),
            )
            .limit(2)
            .send()
            .await
            .map_err(|source| EngineerAuthError::QueryFailed {
                source: Box::new(source),
            })?;

        let items = output.items();

        if items.len() > 1 {
            return Err(EngineerAuthError::DuplicateApiKeyHash);
        }

        items
            .first()
            .map(authenticated_engineer_from_item)
            .transpose()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct AuthenticatedEngineer {
    pub daily_token_limit: Option<u64>,
    pub enabled: bool,
    pub user_id: String,
    pub weekly_token_limit: Option<u64>,
}

pub(crate) fn authenticated_engineer_from_item(
    item: &HashMap<String, AttributeValue>,
) -> Result<AuthenticatedEngineer, EngineerAuthError> {
    let daily_token_limit = optional_number_attribute(item, DAILY_TOKEN_LIMIT_ATTRIBUTE)?;
    let enabled = required_bool_attribute(item, ENABLED_ATTRIBUTE)?;
    let user_id = required_string_attribute(item, USER_ID_ATTRIBUTE)?;
    let weekly_token_limit = optional_number_attribute(item, WEEKLY_TOKEN_LIMIT_ATTRIBUTE)?;

    Ok(AuthenticatedEngineer {
        daily_token_limit,
        enabled,
        user_id,
        weekly_token_limit,
    })
}

fn required_bool_attribute(
    item: &HashMap<String, AttributeValue>,
    attribute_name: &'static str,
) -> Result<bool, EngineerAuthError> {
    match item.get(attribute_name) {
        Some(AttributeValue::Bool(value)) => Ok(*value),
        _ => Err(EngineerAuthError::InvalidEngineerItem {
            missing_attribute: attribute_name,
        }),
    }
}

fn required_string_attribute(
    item: &HashMap<String, AttributeValue>,
    attribute_name: &'static str,
) -> Result<String, EngineerAuthError> {
    match item.get(attribute_name) {
        Some(AttributeValue::S(value)) if !value.is_empty() => Ok(value.clone()),
        _ => Err(EngineerAuthError::InvalidEngineerItem {
            missing_attribute: attribute_name,
        }),
    }
}

fn optional_number_attribute(
    item: &HashMap<String, AttributeValue>,
    attribute_name: &'static str,
) -> Result<Option<u64>, EngineerAuthError> {
    match item.get(attribute_name) {
        Some(AttributeValue::N(value)) => {
            value
                .parse::<u64>()
                .map(Some)
                .map_err(|_| EngineerAuthError::InvalidEngineerItem {
                    missing_attribute: attribute_name,
                })
        }
        Some(_) => Err(EngineerAuthError::InvalidEngineerItem {
            missing_attribute: attribute_name,
        }),
        None => Ok(None),
    }
}

#[derive(Debug)]
pub enum EngineerAuthError {
    QueryFailed {
        source: Box<dyn Error + Send + Sync + 'static>,
    },
    DuplicateApiKeyHash,
    InvalidEngineerItem {
        missing_attribute: &'static str,
    },
}

impl Display for EngineerAuthError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueryFailed { source } => {
                write!(
                    formatter,
                    "failed to query engineer by api key hash: {source}"
                )
            }
            Self::DuplicateApiKeyHash => {
                write!(
                    formatter,
                    "multiple engineers matched the same api key hash"
                )
            }
            Self::InvalidEngineerItem { missing_attribute } => {
                write!(
                    formatter,
                    "engineer auth item is missing valid {missing_attribute}"
                )
            }
        }
    }
}

impl Error for EngineerAuthError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::QueryFailed { source } => Some(source.as_ref()),
            Self::DuplicateApiKeyHash | Self::InvalidEngineerItem { .. } => None,
        }
    }
}
