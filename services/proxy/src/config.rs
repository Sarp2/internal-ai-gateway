use std::env;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;

const DEFAULT_PORT: u16 = 8080;
const DEFAULT_MAX_ACTIVE_STREAMS: usize = 200;
const DEFAULT_METRIC_INTERVAL_SECONDS: u64 = 15;

#[derive(Debug)]
pub struct ProxyConfig {
    pub engineers_api_key_index_name: String,
    pub engineers_table_name: String,
    pub port: u16,
    pub max_active_streams: usize,
    pub metric_interval: Duration,
    pub proxy_api_key_hash_secret_arn: String,
}

impl ProxyConfig {
    pub fn from_env() -> Result<Self, ProxyConfigError> {
        Self::from_values(|name| env::var(name).ok())
    }

    pub(crate) fn from_values(
        read_value: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, ProxyConfigError> {
        Ok(Self {
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
            proxy_api_key_hash_secret_arn: required_value(
                read_value("PROXY_API_KEY_HASH_SECRET_ARN"),
                "PROXY_API_KEY_HASH_SECRET_ARN",
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
