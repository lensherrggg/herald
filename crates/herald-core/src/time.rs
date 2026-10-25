//! Timestamp formatting for the human-facing surfaces.
//!
//! Mail headers and sync bookkeeping are stored as RFC 3339, often in UTC or
//! the sender's offset. CLI/TUI display everything in China Standard Time so a
//! mailbox spanning time zones still reads consistently.

use chrono::{DateTime, FixedOffset};

/// UTC+8 (Asia/Shanghai has no daylight saving time).
pub const CHINA_OFFSET_SECONDS: i32 = 8 * 3600;

fn offset() -> FixedOffset {
    FixedOffset::east_opt(CHINA_OFFSET_SECONDS).expect("UTC+8 is a valid offset")
}

fn parse(value: &str) -> Option<DateTime<FixedOffset>> {
    let value = value.trim();
    DateTime::parse_from_rfc3339(value)
        .or_else(|_| DateTime::parse_from_rfc2822(value))
        .ok()
        .map(|time| time.with_timezone(&offset()))
}

fn raw(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// `YYYY-MM-DD`, used by the TUI list.
pub fn china_date(value: Option<&str>) -> String {
    let Some(value) = raw(value) else {
        return "-".to_string();
    };
    match parse(value) {
        Some(time) => time.format("%Y-%m-%d").to_string(),
        None if value.len() >= 10 && value.as_bytes().get(4) == Some(&b'-') => {
            value[..10].to_string()
        }
        None => value.to_string(),
    }
}

/// `YYYY-MM-DD HH:MM:SS`, used by detail views and CLI output.
pub fn china_datetime(value: Option<&str>) -> String {
    let Some(value) = raw(value) else {
        return "-".to_string();
    };
    match parse(value) {
        Some(time) => time.format("%Y-%m-%d %H:%M:%S").to_string(),
        None => value.to_string(),
    }
}

/// `MM-DD HH:MM`, used by the compact TUI header.
pub fn china_short(value: Option<&str>) -> String {
    let Some(value) = raw(value) else {
        return "-".to_string();
    };
    match parse(value) {
        Some(time) => time.format("%m-%d %H:%M").to_string(),
        None => value.chars().take(16).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_utc_and_sender_offsets_to_china_time() {
        assert_eq!(china_date(Some("2024-09-03T20:00:00Z")), "2024-09-04");
        assert_eq!(
            china_datetime(Some("2024-09-03T20:00:00Z")),
            "2024-09-04 04:00:00"
        );
        assert_eq!(
            china_datetime(Some("Tue, 03 Sep 2024 20:00:00 -0500")),
            "2024-09-04 09:00:00"
        );
        assert_eq!(
            china_datetime(Some("2024-09-03T10:00:00+08:00")),
            "2024-09-03 10:00:00"
        );
        assert_eq!(china_short(Some("2024-09-03T20:00:00Z")), "09-04 04:00");
    }

    #[test]
    fn keeps_unknown_formats_readable() {
        assert_eq!(china_date(None), "-");
        assert_eq!(china_datetime(Some("  ")), "-");
        assert_eq!(china_date(Some("2024-09-03")), "2024-09-03");
        assert_eq!(china_datetime(Some("not a date")), "not a date");
    }
}
