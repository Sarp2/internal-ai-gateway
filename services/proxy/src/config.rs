use std::env;
use std::time::Duration;

const DEFAULT_PORT: u16 = 8080;
const DEFAULT_MAX_ACTIVE_STREAMS: usize = 200;
const DEFAULT_METRIC_INTERVAL_SECONDS: u64 = 15;

pub struct ProxyConfig {
    pub port: u16,
    pub max_active_streams: usize,
    pub metric_interval: Duration,
}

impl ProxyConfig {
    pub fn from_env() -> Self {
        Self::from_values(|name| env::var(name).ok())
    }

    pub(crate) fn from_values(read_value: impl Fn(&str) -> Option<String>) -> Self {
        Self {
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
        }
    }
}

fn parse_value<T>(value: Option<String>, default_value: T) -> T
where
    T: std::str::FromStr,
{
    value
        .and_then(|raw_value| raw_value.parse::<T>().ok())
        .unwrap_or(default_value)
}
