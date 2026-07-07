use std::time::Duration;

use crate::rate_limit::RateLimitError;
use crate::rate_limit::rate_limit_ttl_epoch_seconds;
use crate::rate_limit::window_start_epoch_seconds;

#[test]
fn calculates_window_start_for_request_time() {
    assert_eq!(
        window_start_epoch_seconds(125, Duration::from_secs(60)),
        120
    );
}

#[test]
fn clamps_zero_second_window_when_calculating_window_start() {
    assert_eq!(window_start_epoch_seconds(125, Duration::from_secs(0)), 125);
}

#[test]
fn keeps_rate_limit_records_for_two_windows() {
    assert_eq!(
        rate_limit_ttl_epoch_seconds(120, Duration::from_secs(60)),
        240
    );
}

#[test]
fn saturates_ttl_when_window_is_too_large() {
    assert_eq!(
        rate_limit_ttl_epoch_seconds(u64::MAX - 1, Duration::from_secs(u64::MAX)),
        u64::MAX
    );
}

#[test]
fn describes_rate_limit_exceeded() {
    assert_eq!(
        RateLimitError::RateLimitExceeded.to_string(),
        "rate limit exceeded"
    );
}
