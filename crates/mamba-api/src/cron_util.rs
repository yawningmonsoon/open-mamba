//! Cron expression parsing + next-run computation.
//!
//! Wraps the `cron` crate. Accepts standard 5-field expressions
//! (`min hour dom month dow`) in UTC. We don't expose a 6-field or
//! seconds variant — n8n/most users send the standard 5 fields.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use cron::Schedule;
use std::str::FromStr;

/// Parse a 5-field cron expression and return the next firing time
/// strictly after `after`.
pub fn next_after(expr: &str, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let trimmed = expr.trim();
    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(anyhow!(
            "expected 5 cron fields (min hour dom month dow), got {} in {:?}",
            fields.len(),
            trimmed
        ));
    }
    // The `cron` crate wants 7 fields (seconds and year); pad them.
    let normalized = format!("0 {trimmed} *");
    let schedule = Schedule::from_str(&normalized)
        .map_err(|e| anyhow!("invalid cron expression {:?}: {e}", trimmed))?;
    schedule
        .after(&after)
        .next()
        .ok_or_else(|| anyhow!("cron schedule produced no future time"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn every_minute() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 30, 15).unwrap();
        let next = next_after("* * * * *", now).unwrap();
        assert_eq!(next.minute(), 31);
        assert_eq!(next.hour(), 12);
    }

    #[test]
    fn daily_at_three_am() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let next = next_after("0 3 * * *", now).unwrap();
        assert_eq!(next.hour(), 3);
        assert_eq!(next.day(), 2); // tomorrow
    }

    #[test]
    fn rejects_nonsense() {
        assert!(next_after("not a cron", Utc::now()).is_err());
        assert!(next_after("0 25 * * *", Utc::now()).is_err()); // hour=25
    }

    #[test]
    fn requires_five_fields() {
        assert!(next_after("0 0 0", Utc::now()).is_err());
        assert!(next_after("0 0 * * * *", Utc::now()).is_err()); // 6 fields
    }

    use chrono::Datelike;
    use chrono::Timelike;
}
