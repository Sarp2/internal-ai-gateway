use std::env;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;

const DEFAULT_PORT: u16 = 8080;
const DEFAULT_MAX_ACTIVE_STREAMS: usize = 200;
const DEFAULT_METRIC_INTERVAL_SECONDS: u64 = 15;
const DEFAULT_RATE_LIMIT_REQUESTS_PER_WINDOW: u64 = 120;
const DEFAULT_RATE_LIMIT_WINDOW_SECONDS: u64 = 60;

#[derive(Debug)]
pub struct ProxyConfig {
    pub anthropic_api_key_secret_arn: String,
    pub engineers_api_key_index_name: String,
    pub engineers_table_name: String,
    pub port: u16,
    pub max_active_streams: usize,
    pub metric_interval: Duration,
    pub openai_api_key_secret_arn: String,
    pub proxy_api_key_hash_secret_arn: String,
    pub rate_limit_requests_per_window: u64,
    pub rate_limit_table_name: String,
    pub rate_limit_window: Duration,
    pub token_usage_table_name: String,
}

impl ProxyConfig {
    pub fn from_env() -> Result<Self, ProxyConfigError> {
        Self::from_values(|name| env::var(name).ok())
    }

    pub(crate) fn from_values(
        read_value: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProxyConfigError> {
        Ok(Self {
            anthropic_api_key_secret_arn: required_value(
                read_value("ANTHROPIC_API_KEY_SECRET_ARN"),
                "ANTHROPIC_API_KEY_SECRET_ARN",
            )?,
            engineers_api_key_index_name: required_value(
                read_value("ENGINEERS_API_KEY_INDEX_NAME"),
                "ENGINEERS_API_KEY_INDEX_NAME",
            )?,
            engineers_table_name: required_value(
                read_value("ENGINEERS_TABLE_NAME"),
                "ENGINEERS_TABLE_NAME",
            )?,
            port: parse_value(read_value("PORT"), DEFAULT_PORT),
            max_active_streams: parse_value(
                read_value("MAX_ACTIVE_STREAMS"),
                DEFAULT_MAX_ACTIVE_STREAMS,
            )
            .max(1),
            metric_interval: Duration::from_secs(
                parse_value(
                    read_value("ACTIVE_STREAM_METRIC_INTERVAL_SECONDS"),
                    DEFAULT_METRIC_INTERVAL_SECONDS,
                )
                .max(1),
            ),
            openai_api_key_secret_arn: required_value(
                read_value("OPENAI_API_KEY_SECRET_ARN"),
                "OPENAI_API_KEY_SECRET_ARN",
            )?,
            proxy_api_key_hash_secret_arn: required_value(
                read_value("PROXY_API_KEY_HASH_SECRET_ARN"),
                "PROXY_API_KEY_HASH_SECRET_ARN",
            )?,
            rate_limit_requests_per_window: parse_value(
                read_value("RATE_LIMIT_REQUESTS_PER_WINDOW"),
                DEFAULT_RATE_LIMIT_REQUESTS_PER_WINDOW,
            )
            .max(1),
            rate_limit_table_name: required_value(
                read_value("RATE_LIMIT_TABLE_NAME"),
                "RATE_LIMIT_TABLE_NAME",
            )?,
            rate_limit_window: Duration::from_secs(
                parse_value(
                    read_value("RATE_LIMIT_WINDOW_SECONDS"),
                    DEFAULT_RATE_LIMIT_WINDOW_SECONDS,
                )
                .max(1),
            ),
            token_usage_table_name: required_value(
                read_value("TOKEN_USAGE_TABLE_NAME"),
                "TOKEN_USAGE_TABLE_NAME",
            )?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProxyConfigError {
    MissingRequiredValue(&'static str),
}

impl Display for ProxyConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingRequiredValue(name) => {
                write!(formatter, "missing required environment value {name}")
            }
        }
    }
}

impl Error for ProxyConfigError {}

fn parse_value<T>(value: Option<String>, default_value: T) -> T
where
    T: std::str::FromStr,
{
    value
        .and_then(|raw_value| raw_value.parse::<T>().ok())
        .unwrap_or(default_value)
}

fn required_value(value: Option<String>, name: &'static str) -> Result<String, ProxyConfigError> {
    value
        .filter(|raw_value| !raw_value.trim().is_empty())
        .ok_or(ProxyConfigError::MissingRequiredValue(name))
}
