//! Calendar arithmetic in a time zone.
//!
//! Dates are `NaiveDate`s and wall-clock times `NaiveDateTime`s — local, with
//! no zone attached — and instants are epoch seconds. Dates only become text
//! at the edges: labels in a response, and the SQL they are matched against,
//! where ISO `YYYY-MM-DD` is how both Plausible and SQLite write them.

use chrono::{Datelike, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use chrono_tz::Tz;

pub fn parse_zone(name: &str) -> Tz {
    name.parse().unwrap_or(Tz::UTC)
}

/// What a wall clock in `tz` shows at `epoch`.
pub fn wall_clock(epoch: i64, tz: Tz) -> NaiveDateTime {
    tz.timestamp_opt(epoch, 0).single().map(|time| time.naive_local()).unwrap_or_default()
}

/// A wall-clock time counted as if it were UTC, which is what arithmetic on
/// labels is done in.
pub fn naive_epoch(time: NaiveDateTime) -> i64 {
    time.and_utc().timestamp()
}

/// Seconds `tz` is ahead of UTC at `epoch`.
pub fn offset_at(epoch: i64, tz: Tz) -> i64 {
    naive_epoch(wall_clock(epoch, tz)) - epoch
}

/// The instant a wall clock in `tz` shows `local`.
///
/// Two passes, so a time on the far side of a daylight-saving change still
/// lands right. A time that never happens — inside a spring-forward gap —
/// lands an hour late, which for the start of a day is the first moment that
/// day has.
pub fn zoned_epoch(local: NaiveDateTime, tz: Tz) -> i64 {
    let naive = naive_epoch(local);
    let first = naive - offset_at(naive, tz);
    naive - offset_at(first, tz)
}

/// The local date `tz` is on at `epoch`.
pub fn zoned_date(epoch: i64, tz: Tz) -> NaiveDate {
    wall_clock(epoch, tz).date()
}

pub fn midnight(date: NaiveDate) -> NaiveDateTime {
    date.and_time(NaiveTime::MIN)
}

/// The first and last second of a local day.
pub fn day_bounds(date: NaiveDate, tz: Tz) -> (i64, i64) {
    (zoned_epoch(midnight(date), tz), zoned_epoch(midnight(add_days(date, 1)), tz) - 1)
}

/// Where the zone's offset changes between two instants, as `(from, offset)`
/// segments. Found by sampling every six hours — no zone changes offset twice
/// in that long — and narrowing each change down to the second. chrono-tz
/// doesn't expose its transitions, or this would read them.
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

pub fn add_days(date: NaiveDate, days: i64) -> NaiveDate {
    date + chrono::Duration::days(days)
}

/// Days from `from` to `to`.
pub fn days_between(from: NaiveDate, to: NaiveDate) -> i64 {
    (to - from).num_days()
}

/// ISO weekday: 1 for Monday through 7 for Sunday.
pub fn day_of_week(date: NaiveDate) -> i64 {
    i64::from(date.weekday().number_from_monday())
}

pub fn beginning_of_week(date: NaiveDate) -> NaiveDate {
    add_days(date, 1 - day_of_week(date))
}

pub fn beginning_of_month(date: NaiveDate) -> NaiveDate {
    date.with_day(1).expect("every month has a first")
}

pub fn end_of_month(date: NaiveDate) -> NaiveDate {
    add_days(add_months(beginning_of_month(date), 1), -1)
}

/// The same day `months` later, clamped to the end of a shorter month — which
/// is how Elixir's `Date.shift` does it, and so how Plausible's periods move.
pub fn add_months(date: NaiveDate, months: i64) -> NaiveDate {
    let shifted = if months >= 0 {
        date.checked_add_months(Months::new(months as u32))
    } else {
        date.checked_sub_months(Months::new(months.unsigned_abs() as u32))
    };
    shifted.unwrap_or(date)
}

pub fn add_years(date: NaiveDate, years: i64) -> NaiveDate {
    add_months(date, years * 12)
}

/// A strict `YYYY-MM-DD` date.
pub fn parse_date(value: &str) -> Option<NaiveDate> {
    let bytes = value.as_bytes();
    let shaped = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes.iter().enumerate().all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
    shaped.then(|| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok()).flatten()
}

/// An instant as ISO 8601 with the zone's offset, the way Plausible reports a
/// query's date range: `2026-08-31T00:00:00+02:00`, or `Z` in UTC itself.
pub fn iso_in_zone(epoch: i64, tz: Tz, name: &str) -> String {
    let clock = wall_clock(epoch, tz).format("%Y-%m-%dT%H:%M:%S");
    if name == "UTC" || name == "Etc/UTC" {
        return format!("{clock}Z");
    }
    let offset = offset_at(epoch, tz);
    let sign = if offset < 0 { '-' } else { '+' };
    let minutes = offset.abs() / 60;
    format!("{clock}{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        parse_date(text).unwrap()
    }

    #[test]
    fn arithmetic() {
        assert_eq!(add_months(date("2026-01-31"), 1), date("2026-02-28"));
        assert_eq!(add_months(date("2024-03-31"), -1), date("2024-02-29"));
        assert_eq!(add_years(date("2024-02-29"), -1), date("2023-02-28"));
        assert_eq!(beginning_of_week(date("2026-09-30")), date("2026-09-28"));
        assert_eq!(end_of_month(date("2026-02-10")), date("2026-02-28"));
        assert_eq!(days_between(date("2026-01-01"), date("2026-03-01")), 59);
        assert!(parse_date("2026-02-30").is_none());
        assert!(parse_date("2026-2-3").is_none());
        assert!(parse_date("0050-01-01").is_some());
    }

    #[test]
    fn zones() {
        let berlin = parse_zone("Europe/Berlin");
        // 2026-03-29 is the spring-forward day in Berlin: 23 hours long.
        let (first, last) = day_bounds(date("2026-03-29"), berlin);
        assert_eq!(last - first + 1, 23 * 3600);
        assert_eq!(iso_in_zone(first, berlin, "Europe/Berlin"), "2026-03-29T00:00:00+01:00");
        assert_eq!(iso_in_zone(0, Tz::UTC, "UTC"), "1970-01-01T00:00:00Z");
        let segments = offset_segments(first, last, berlin);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[1], (first + 2 * 3600, 7200));
    }
}
