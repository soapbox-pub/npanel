//! Calendar arithmetic in a time zone.
//!
//! Dates are ISO `YYYY-MM-DD` strings throughout, which is how Plausible
//! labels them and how SQLite prints them, so a date computed here and a date
//! grouped by in SQL compare as equal strings. Instants are epoch seconds.

use chrono::{Datelike, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;

pub type CivilDate = String;

/// A moment as a wall clock in some zone shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WallClock {
    pub date: CivilDate,
    pub hour: i64,
    pub minute: i64,
    pub second: i64,
}

const DAY: i64 = 86_400;

pub fn parse_zone(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
}

/// What a wall clock in `tz` shows at `epoch`.
pub fn wall_clock(epoch: i64, tz: Tz) -> WallClock {
    let time = tz.timestamp_opt(epoch, 0).single().unwrap_or_else(|| Tz::UTC.timestamp_opt(0, 0).unwrap());
    WallClock {
        date: format_date(time.date_naive()),
        hour: i64::from(time.hour()),
        minute: i64::from(time.minute()),
        second: i64::from(time.second()),
    }
}

/// Seconds `tz` is ahead of UTC at `epoch`.
pub fn offset_at(epoch: i64, tz: Tz) -> i64 {
    let clock = wall_clock(epoch, tz);
    civil_epoch(&clock.date) + clock.hour * 3600 + clock.minute * 60 + clock.second - epoch
}

/// The instant a wall clock in `tz` shows the given time.
///
/// Two passes, so a time on the far side of a daylight-saving change still
/// lands right. A time that never happens — inside a spring-forward gap —
/// lands an hour late, which for the start of a day is the first moment that
/// day has.
pub fn zoned_epoch(date: &str, tz: Tz, hour: i64, minute: i64, second: i64) -> i64 {
    let naive = civil_epoch(date) + hour * 3600 + minute * 60 + second;
    let first = naive - offset_at(naive, tz);
    naive - offset_at(first, tz)
}

/// The local date `tz` is on at `epoch`.
pub fn zoned_date(epoch: i64, tz: Tz) -> CivilDate {
    wall_clock(epoch, tz).date
}

/// The first and last second of a local day.
pub fn day_bounds(date: &str, tz: Tz) -> (i64, i64) {
    (zoned_epoch(date, tz, 0, 0, 0), zoned_epoch(&add_days(date, 1), tz, 0, 0, 0) - 1)
}

/// Where the zone's offset changes between two instants, as `(from, offset)`
/// segments. Found by sampling every six hours — no zone changes offset twice
/// in that long — and narrowing each change down to the second.
pub fn offset_segments(from: i64, to: i64, tz: Tz) -> Vec<(i64, i64)> {
    let step = 6 * 3600;
    let mut segments = vec![(from, offset_at(from, tz))];

    let mut at = from + step;
    while at < to + step {
        let sample = at.min(to);
        let previous = segments.last().expect("one segment").1;
        let offset = offset_at(sample, tz);
        if offset != previous {
            let (mut low, mut high) = (sample - step, sample);
            while high - low > 1 {
                let middle = (low + high).div_euclid(2);
                if offset_at(middle, tz) == previous {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            segments.push((high, offset));
        }
        at += step;
    }

    segments
}

fn parse_date(date: &str) -> NaiveDate {
    NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap_or_default()
}

pub fn format_date(date: NaiveDate) -> CivilDate {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

/// Midnight UTC of a date, which is what civil arithmetic is done in.
pub fn civil_epoch(date: &str) -> i64 {
    parse_date(date).and_hms_opt(0, 0, 0).map(|t| t.and_utc().timestamp()).unwrap_or(0)
}

pub fn from_civil_epoch(epoch: i64) -> CivilDate {
    format_date(chrono::DateTime::from_timestamp(epoch, 0).unwrap_or_default().date_naive())
}

pub fn add_days(date: &str, days: i64) -> CivilDate {
    from_civil_epoch(civil_epoch(date) + days * DAY)
}

/// Days from `from` to `to`.
pub fn days_between(from: &str, to: &str) -> i64 {
    (civil_epoch(to) - civil_epoch(from)).div_euclid(DAY)
}

/// ISO weekday: 1 for Monday through 7 for Sunday.
pub fn day_of_week(date: &str) -> i64 {
    i64::from(parse_date(date).weekday().number_from_monday())
}

pub fn beginning_of_week(date: &str) -> CivilDate {
    add_days(date, 1 - day_of_week(date))
}

pub fn beginning_of_month(date: &str) -> CivilDate {
    format!("{}-01", &date[..7])
}

pub fn end_of_month(date: &str) -> CivilDate {
    add_days(&add_months(&beginning_of_month(date), 1), -1)
}

/// The same day `months` later, clamped to the end of a shorter month — which
/// is how Elixir's `Date.shift` does it, and so how Plausible's periods move.
pub fn add_months(date: &str, months: i64) -> CivilDate {
    let d = parse_date(date);
    let index = i64::from(d.year()) * 12 + i64::from(d.month0()) + months;
    let (year, month) = (index.div_euclid(12) as i32, index.rem_euclid(12) as u32 + 1);
    let last = NaiveDate::from_ymd_opt(year, month, 1)
        .and_then(|first| first.checked_add_months(chrono::Months::new(1)))
        .and_then(|next| next.pred_opt())
        .map_or(28, |last| last.day());
    format_date(NaiveDate::from_ymd_opt(year, month, d.day().min(last)).unwrap_or_default())
}

pub fn add_years(date: &str, years: i64) -> CivilDate {
    add_months(date, years * 12)
}

/// Whether a string is a real `YYYY-MM-DD` date.
pub fn is_civil_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit())
        && NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

/// An instant as ISO 8601 with the zone's offset, the way Plausible reports a
/// query's date range: `2026-08-31T00:00:00+02:00`, or `Z` in UTC itself.
pub fn iso_in_zone(epoch: i64, tz: Tz, name: &str) -> String {
    let clock = wall_clock(epoch, tz);
    let time = format!("{:02}:{:02}:{:02}", clock.hour, clock.minute, clock.second);
    if name == "UTC" || name == "Etc/UTC" {
        return format!("{}T{time}Z", clock.date);
    }
    let offset = offset_at(epoch, tz);
    let sign = if offset < 0 { '-' } else { '+' };
    let minutes = offset.abs() / 60;
    format!("{}T{time}{sign}{:02}:{:02}", clock.date, minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic() {
        assert_eq!(add_months("2026-01-31", 1), "2026-02-28");
        assert_eq!(add_months("2024-03-31", -1), "2024-02-29");
        assert_eq!(add_years("2024-02-29", -1), "2023-02-28");
        assert_eq!(beginning_of_week("2026-09-30"), "2026-09-28");
        assert_eq!(end_of_month("2026-02-10"), "2026-02-28");
        assert_eq!(days_between("2026-01-01", "2026-03-01"), 59);
        assert!(is_civil_date("2026-02-28"));
        assert!(!is_civil_date("2026-02-30"));
        assert!(is_civil_date("0050-01-01"));
    }

    #[test]
    fn zones() {
        let berlin = parse_zone("Europe/Berlin");
        // 2026-03-29 is the spring-forward day in Berlin: 23 hours long.
        let (first, last) = day_bounds("2026-03-29", berlin);
        assert_eq!(last - first + 1, 23 * 3600);
        assert_eq!(iso_in_zone(first, berlin, "Europe/Berlin"), "2026-03-29T00:00:00+01:00");
        assert_eq!(iso_in_zone(0, Tz::UTC, "UTC"), "1970-01-01T00:00:00Z");
        let segments = offset_segments(first, last, berlin);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[1], (first + 2 * 3600, 7200));
    }
}
