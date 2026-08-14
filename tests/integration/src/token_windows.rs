use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::time::sleep;

use crate::fixtures::engineer::FixtureError;

const DAILY_WINDOW_SECONDS: u64 = 86_400;
const MONDAY_WEEK_OFFSET_SECONDS: u64 = DAILY_WINDOW_SECONDS * 3;
const TOKEN_WINDOW_SAFETY_MARGIN: Duration = Duration::from_secs(120);
const WEEKLY_WINDOW_SECONDS: u64 = DAILY_WINDOW_SECONDS * 7;

pub async fn stable_usage_window_time() -> Result<u64, FixtureError> {
    loop {
        let current_time = current_epoch_seconds()?;
        let daily_remaining = seconds_until_window_rollover(current_time, DAILY_WINDOW_SECONDS);
        let weekly_remaining = seconds_until_window_rollover(
            current_time.saturating_add(MONDAY_WEEK_OFFSET_SECONDS),
            WEEKLY_WINDOW_SECONDS,
        );
        let wait_seconds = [daily_remaining, weekly_remaining]
            .into_iter()
            .filter(|remaining| *remaining <= TOKEN_WINDOW_SAFETY_MARGIN.as_secs())
            .max();

        match wait_seconds {
            Some(remaining) => sleep(Duration::from_secs(remaining + 1)).await,
            None => return Ok(current_time),
        }
    }
}

pub fn daily_usage_window(epoch_seconds: u64) -> String {
    format!("daily#{}", epoch_seconds / DAILY_WINDOW_SECONDS)
}

pub fn weekly_usage_window(epoch_seconds: u64) -> String {
    format!(
        "weekly#{}",
        epoch_seconds.saturating_add(MONDAY_WEEK_OFFSET_SECONDS) / WEEKLY_WINDOW_SECONDS
    )
}

fn current_epoch_seconds() -> Result<u64, FixtureError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn seconds_until_window_rollover(epoch_seconds: u64, window_seconds: u64) -> u64 {
    window_seconds - (epoch_seconds % window_seconds)
}
