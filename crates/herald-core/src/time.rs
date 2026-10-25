//! Timestamp formatting for the human-facing surfaces.
//!
//! Mail headers and sync bookkeeping are stored as RFC 3339, often in UTC or
//! the sender's offset. CLI/TUI display everything in China Standard Time so a
//! mailbox spanning time zones still reads consistently.

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, Utc, Weekday};

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

fn parse_date(value: &str) -> Option<NaiveDate> {
    parse(value).map(|time| time.date_naive()).or_else(|| {
        value
            .get(..10)
            .and_then(|head| NaiveDate::parse_from_str(head, "%Y-%m-%d").ok())
    })
}

fn weekday_label(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Mon => "周一",
        Weekday::Tue => "周二",
        Weekday::Wed => "周三",
        Weekday::Thu => "周四",
        Weekday::Fri => "周五",
        Weekday::Sat => "周六",
        Weekday::Sun => "周日",
    }
}

/// Compact label for the TUI mail list, anchored on the current week:
/// this week → `周一`…`周日`, this year → `MM-DD`, older → `YY-MM-DD`.
/// Keeping the year to two digits leaves more room for the subject column.
pub fn china_list_date(value: Option<&str>) -> String {
    list_date(value, Utc::now().with_timezone(&offset()).date_naive())
}

/// [`china_list_date`] with an explicit "today", so the weekly boundary is
/// testable without depending on the clock.
pub fn list_date(value: Option<&str>, today: NaiveDate) -> String {
    let Some(value) = raw(value) else {
        return "-".to_string();
    };
    let Some(date) = parse_date(value) else {
        return value.to_string();
    };
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let sunday = monday + Duration::days(6);
    if date >= monday && date <= sunday {
        weekday_label(date.weekday()).to_string()
    } else if date.year() == today.year() {
        date.format("%m-%d").to_string()
    } else {
        date.format("%y-%m-%d").to_string()
    }
}

/// Current date in China Standard Time.
pub fn china_today() -> NaiveDate {
    Utc::now().with_timezone(&offset()).date_naive()
}

/// Parse a stored timestamp into its China-local calendar date.
pub fn china_date(value: Option<&str>) -> Option<NaiveDate> {
    raw(value).and_then(parse_date)
}

/// `HH:MM` in China Standard Time; `-` when the value is missing or invalid.
pub fn china_clock(value: Option<&str>) -> String {
    let Some(value) = raw(value) else {
        return "-".to_string();
    };
    match parse(value) {
        Some(time) => time.format("%H:%M").to_string(),
        None => "-".to_string(),
    }
}

/// Stable key for grouping consecutive messages by China-local day. Invalid
/// timestamps stay distinct so unrelated malformed dates do not get merged.
pub fn china_group_key(value: Option<&str>) -> String {
    match raw(value).and_then(parse_date) {
        Some(date) => date.format("%Y-%m-%d").to_string(),
        None => raw(value).unwrap_or("未知日期").to_string(),
    }
}

/// Heading for a date group in the mail list, e.g. `今天 10-25`,
/// `昨天 10-24`, `周三 10-21`, `09-12` or `24-09-12`. Older years use the
/// same two-digit form as the compact row meta so switching modes keeps
/// the date column consistent.
pub fn list_group(value: Option<&str>, today: NaiveDate) -> String {
    let Some(value) = raw(value) else {
        return "未知日期".to_string();
    };
    let Some(date) = parse_date(value) else {
        return value.to_string();
    };
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    let sunday = monday + Duration::days(6);
    if date == today {
        format!("今天 {}", date.format("%m-%d"))
    } else if date == today - Duration::days(1) {
        format!("昨天 {}", date.format("%m-%d"))
    } else if date >= monday && date <= sunday {
        format!("{} {}", weekday_label(date.weekday()), date.format("%m-%d"))
    } else if date.year() == today.year() {
        date.format("%m-%d").to_string()
    } else {
        date.format("%y-%m-%d").to_string()
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
    fn list_date_prefers_weekday_then_compact_year() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let label = |value| list_date(Some(value), today);

        assert_eq!(label("2026-10-19T09:00:00+08:00"), "周一");
        assert_eq!(label("2026-10-25T23:00:00+08:00"), "周日");
        assert_eq!(
            label("2026-10-18T16:30:00Z"),
            "周一",
            "UTC Sunday evening is Monday in China"
        );
        assert_eq!(label("2026-10-18T23:00:00+08:00"), "10-18");
        assert_eq!(label("2026-01-01T10:00:00+08:00"), "01-01");
        assert_eq!(label("2025-12-31T10:00:00+08:00"), "25-12-31");
        assert_eq!(label("2024-09-03"), "24-09-03");
        assert_eq!(label("not a date"), "not a date");
        assert_eq!(list_date(None, today), "-");
    }

    #[test]
    fn keeps_unknown_formats_readable() {
        assert_eq!(china_datetime(Some("  ")), "-");
        assert_eq!(china_datetime(Some("not a date")), "not a date");
    }

    #[test]
    fn clock_and_group_labels_use_china_time() {
        assert_eq!(china_clock(Some("2026-10-25T01:05:00Z")), "09:05");
        assert_eq!(china_clock(Some("2026-10-25T09:05:00+08:00")), "09:05");
        assert_eq!(china_clock(None), "-");
        assert_eq!(china_clock(Some("not a date")), "-");

        let today = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        let group = |value| list_group(Some(value), today);
        assert_eq!(group("2026-10-25T01:00:00Z"), "今天 10-25");
        assert_eq!(group("2026-10-24T10:00:00+08:00"), "昨天 10-24");
        assert_eq!(group("2026-10-21T10:00:00+08:00"), "周三 10-21");
        assert_eq!(group("2026-10-18T10:00:00+08:00"), "10-18");
        assert_eq!(group("2025-12-31T10:00:00+08:00"), "25-12-31");
        assert_eq!(group("not a date"), "not a date");
        assert_eq!(list_group(None, today), "未知日期");
    }

    #[test]
    fn group_keys_normalize_offsets_and_keep_invalid_values_distinct() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 25).unwrap();
        assert_eq!(
            china_group_key(Some("2026-10-24T23:30:00Z")),
            "2026-10-25",
            "UTC late evening is the next day in China"
        );
        assert_eq!(
            china_group_key(Some("2026-10-25T09:05:00+08:00")),
            "2026-10-25"
        );
        assert_eq!(china_group_key(Some("bad one")), "bad one");
        assert_eq!(china_group_key(Some("bad two")), "bad two");
        assert_eq!(china_group_key(None), "未知日期");
        assert_eq!(china_date(Some("2026-10-25T09:05:00+08:00")), Some(today));
        assert_eq!(china_date(Some("bad")), None);
    }
}
