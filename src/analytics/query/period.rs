//! The periods Plausible's dashboard offers, and how each turns into a range
//! of time — a port of `Stats.QueryPeriod`, `Stats.Comparisons` and
//! `Stats.Time`, down to their quirks, so that the same query asks the same
//! question of npanel as of Plausible.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Timelike};
use chrono_tz::Tz;

use super::zone::*;

/// A period the dashboard names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Realtime,
    Realtime30m,
    Day,
    Hours24,
    /// The last few whole days: 7, 28, 30 or 91.
    Days(i64),
    Month,
    /// The last few whole months: 6 or 12.
    Months(i64),
    Year,
    All,
}

impl Period {
    pub const ALL: [Period; 13] = [
        Period::Realtime,
        Period::Realtime30m,
        Period::Day,
        Period::Hours24,
        Period::Days(7),
        Period::Days(28),
        Period::Days(30),
        Period::Days(91),
        Period::Month,
        Period::Months(6),
        Period::Months(12),
        Period::Year,
        Period::All,
    ];

    pub fn parse(name: &str) -> Option<Period> {
        Period::ALL.into_iter().find(|period| period.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Period::Realtime => "realtime",
            Period::Realtime30m => "realtime_30m",
            Period::Day => "day",
            Period::Hours24 => "24h",
            Period::Days(7) => "7d",
            Period::Days(28) => "28d",
            Period::Days(30) => "30d",
            Period::Days(_) => "91d",
            Period::Month => "month",
            Period::Months(6) => "6mo",
            Period::Months(_) => "12mo",
            Period::Year => "year",
            Period::All => "all",
        }
    }
}

/// A named period, or a custom one from one date to another, inclusive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DateRange {
    Period(Period),
    Custom(NaiveDate, NaiveDate),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Compare {
    PreviousPeriod,
    YearOverYear,
    Custom(NaiveDate, NaiveDate),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interval {
    Minute,
    Hour,
    Day,
    Week,
    Month,
}

impl Interval {
    pub const ALL: [Interval; 5] = [Interval::Minute, Interval::Hour, Interval::Day, Interval::Week, Interval::Month];

    pub fn name(self) -> &'static str {
        match self {
            Interval::Minute => "minute",
            Interval::Hour => "hour",
            Interval::Day => "day",
            Interval::Week => "week",
            Interval::Month => "month",
        }
    }

    /// A bucket's label, as Plausible writes it and SQLite groups by it.
    pub fn label(self, bucket: NaiveDateTime) -> String {
        match self {
            Interval::Minute => bucket.format("%Y-%m-%d %H:%M:00").to_string(),
            Interval::Hour => bucket.format("%Y-%m-%d %H:00:00").to_string(),
            Interval::Day | Interval::Week | Interval::Month => bucket.format("%Y-%m-%d").to_string(),
        }
    }
}

/// A span of time, as epoch seconds, both ends included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeRange {
    pub first: i64,
    pub last: i64,
}

/// What a period is resolved against.
pub struct PeriodContext {
    pub tz: Tz,
    pub now: i64,
    /// The day the dashboard is looking from; today unless it says otherwise.
    pub relative_date: NaiveDate,
    /// The first day the site has data, which is where `all` begins.
    pub stats_start: Option<NaiveDate>,
}

/// From the start of one local day to the end of another.
fn dates(first: NaiveDate, last: NaiveDate, tz: Tz) -> TimeRange {
    TimeRange { first: day_bounds(first, tz).0, last: day_bounds(last, tz).1 }
}

/// The time a query covers, trimmed the way the dashboard asks for: an
/// in-progress month or year ends today, and today ends at the end of the
/// current hour unless it is being compared.
pub fn main_range(input: &DateRange, ctx: &PeriodContext, compare: Option<&Compare>) -> TimeRange {
    let (tz, now, relative) = (ctx.tz, ctx.now, ctx.relative_date);
    let today = zoned_date(now, tz);

    let period = match *input {
        DateRange::Custom(first, last) => return dates(first, last, tz),
        DateRange::Period(period) => period,
    };

    match period {
        Period::Realtime => TimeRange { first: now - 5 * 60, last: now + 5 },
        Period::Realtime30m => TimeRange { first: now - 30 * 60, last: now + 5 },
        Period::Hours24 => TimeRange { first: now - 24 * 3600, last: now },
        Period::Day => {
            if relative != today {
                return dates(relative, relative, tz);
            }
            if compare.is_some() {
                return TimeRange { first: day_bounds(today, tz).0, last: now };
            }
            let hour = wall_clock(now, tz).hour();
            let end_of_hour = today.and_hms_opt(hour, 59, 59).expect("a real time");
            TimeRange { first: day_bounds(today, tz).0, last: zoned_epoch(end_of_hour, tz) }
        }
        Period::Days(n) => dates(add_days(relative, -n), add_days(relative, -1), tz),
        Period::Months(n) => dates(beginning_of_month(add_months(relative, -n)), end_of_month(add_months(relative, -1)), tz),
        Period::Month => {
            let first = beginning_of_month(relative);
            let last = if first == beginning_of_month(today) { today } else { end_of_month(relative) };
            dates(first, last, tz)
        }
        Period::Year => {
            let first = relative.with_ordinal(1).expect("every year has a first day");
            let last = if first.year() == today.year() {
                today
            } else {
                NaiveDate::from_ymd_opt(relative.year(), 12, 31).expect("every year has a last day")
            };
            dates(first, last, tz)
        }
        Period::All => dates(ctx.stats_start.unwrap_or(relative), relative, tz),
    }
}

/// The range a query is compared against.
///
/// Today and the last 24 hours compare instant for instant — the same hours a
/// day or a week earlier. Everything else compares whole days, measured from
/// the part of the range that has already happened.
pub fn comparison_range(input: &DateRange, main: TimeRange, compare: &Compare, match_day_of_week: bool, ctx: &PeriodContext) -> TimeRange {
    let (tz, now) = (ctx.tz, ctx.now);

    if let Compare::Custom(first, last) = *compare {
        return dates(first, last, tz);
    }

    let today = zoned_date(now, tz);
    let by_instant = match input {
        DateRange::Period(Period::Hours24) => true,
        DateRange::Period(Period::Day) => zoned_date(main.first, tz) == today,
        _ => false,
    };

    if by_instant {
        if *compare == Compare::PreviousPeriod {
            let shift = if match_day_of_week { 7 * 86_400 } else { 86_400 };
            return TimeRange { first: main.first - shift, last: main.last - shift };
        }

        let first = shift_year(main.first, tz);
        let last = shift_year(main.last, tz);
        if !match_day_of_week {
            return TimeRange { first, last };
        }

        let source_date = zoned_date(main.first, tz);
        let comparison_date = zoned_date(first, tz);
        let days = days_between(comparison_date, shift_to_nearest(day_of_week(source_date), comparison_date, source_date));
        return TimeRange { first: first + days * 86_400, last: last + days * 86_400 };
    }

    let (source_first, source_last) = date_range(main, tz);
    let source_last = if source_last > today { source_first.max(today) } else { source_last };

    let (mut first, mut last) = if *compare == Compare::PreviousPeriod {
        let shift = days_between(source_last, source_first) - 1;
        (add_days(source_first, shift), add_days(source_last, shift))
    } else {
        let first = add_years(source_first, -1);
        (first, add_days(first, days_between(source_first, source_last)))
    };

    if match_day_of_week {
        let matched = shift_to_nearest(day_of_week(source_first), first, source_first);
        last = add_days(last, days_between(first, matched));
        first = matched;
    }

    dates(first, last, tz)
}

/// The local dates a range starts and ends on.
pub fn date_range(range: TimeRange, tz: Tz) -> (NaiveDate, NaiveDate) {
    (zoned_date(range.first, tz), zoned_date(range.last, tz))
}

/// The buckets a time dimension divides a range into, each as the local time
/// it starts — except that a week cut short by the range starts where the
/// range does, as Plausible labels it. Every bucket is listed, including the
/// empty ones.
pub fn time_buckets(interval: Interval, range: TimeRange, tz: Tz, now: i64) -> Vec<NaiveDateTime> {
    let (first, last) = date_range(range, tz);
    let mut buckets = Vec::new();

    match interval {
        Interval::Month => {
            let mut month = beginning_of_month(first);
            while month <= last {
                buckets.push(midnight(month));
                month = add_months(month, 1);
            }
        }
        Interval::Week => {
            let weeks = days_between(beginning_of_week(first), last).div_euclid(7);
            for shift in 0..=weeks {
                buckets.push(midnight(date_or_weekstart(add_days(first, shift * 7), first, last)));
            }
        }
        Interval::Day => {
            let mut day = first;
            while day <= last {
                buckets.push(midnight(day));
                day = add_days(day, 1);
            }
        }
        Interval::Hour => {
            let clock = wall_clock(range.first, tz);
            let hour = clock.date().and_hms_opt(clock.hour(), 0, 0).expect("a real time");
            let count = (range.last - zoned_epoch(hour, tz)).div_euclid(3600);
            for step in 0..=count {
                buckets.push(hour + chrono::Duration::hours(step));
            }
        }
        Interval::Minute => {
            let current_minute = now - now.rem_euclid(60);
            let mut at = range.first - range.first.rem_euclid(60);
            while at < range.last && at < current_minute {
                buckets.push(wall_clock(at, tz).with_second(0).expect("a real time"));
                at += 60;
            }
        }
    }

    buckets
}

/// The first and last bucket, when either reaches outside what the range has
/// covered so far. The graph draws those dashed.
pub fn partial_buckets(buckets: &[NaiveDateTime], interval: Interval, range: TimeRange, tz: Tz, now: i64) -> Vec<NaiveDateTime> {
    let (Some(&first), Some(&last)) = (buckets.first(), buckets.last()) else {
        return Vec::new();
    };
    let start = naive_epoch(wall_clock(range.first, tz));
    let cutoff = naive_epoch(wall_clock(now, tz)).min(naive_epoch(wall_clock(range.last, tz)));

    let mut partial = Vec::new();
    if start > bucket_start(first, interval) {
        partial.push(first);
    }
    if bucket_end(last, interval) > cutoff && !partial.contains(&last) {
        partial.push(last);
    }
    partial
}

/// Which bucket now falls in, or `None` if none of them.
pub fn present_index(buckets: &[NaiveDateTime], interval: Interval, range: TimeRange, tz: Tz, now: i64) -> Option<usize> {
    let clock = wall_clock(now, tz);
    let bucket = match interval {
        Interval::Month => midnight(beginning_of_month(clock.date())),
        Interval::Week => {
            let (first, last) = date_range(range, tz);
            midnight(date_or_weekstart(clock.date(), first, last))
        }
        Interval::Day => midnight(clock.date()),
        Interval::Hour => clock.date().and_hms_opt(clock.hour(), 0, 0).expect("a real time"),
        Interval::Minute => clock.with_second(0).expect("a real time"),
    };
    buckets.iter().position(|b| *b == bucket)
}

/// Each hour or minute bucket as the span of instants it covers, for counting
/// a visit in every bucket it was active in.
pub fn bucket_spans(buckets: &[NaiveDateTime], interval: Interval, tz: Tz) -> Vec<(String, i64, i64)> {
    let length = if interval == Interval::Hour { 3600 } else { 60 };
    buckets
        .iter()
        .map(|&bucket| {
            let lo = zoned_epoch(bucket, tz);
            (interval.label(bucket), lo, lo + length)
        })
        .collect()
}

/// A week's label: its Monday, unless that falls outside the range, then the
/// date itself.
pub fn date_or_weekstart(date: NaiveDate, first: NaiveDate, last: NaiveDate) -> NaiveDate {
    let weekstart = beginning_of_week(date);
    if weekstart >= first && weekstart <= last {
        weekstart
    } else {
        date
    }
}

/// The same wall-clock time a year earlier.
fn shift_year(epoch: i64, tz: Tz) -> i64 {
    let clock = wall_clock(epoch, tz);
    zoned_epoch(add_years(clock.date(), -1).and_time(clock.time()), tz)
}

/// The date near `date` on the given weekday, never `reject` — Plausible's
/// `shift_to_nearest`, which tries the next such day before the previous one.
fn shift_to_nearest(weekday: i64, date: NaiveDate, reject: NaiveDate) -> NaiveDate {
    if day_of_week(date) == weekday {
        return date;
    }
    let mut forward = weekday - day_of_week(date);
    if forward <= 0 {
        forward += 7;
    }
    let mut back = day_of_week(date) - weekday;
    if back <= 0 {
        back += 7;
    }
    let next = add_days(date, forward);
    if next != reject {
        next
    } else {
        add_days(date, -back)
    }
}

fn bucket_start(bucket: NaiveDateTime, interval: Interval) -> i64 {
    if interval == Interval::Week {
        return naive_epoch(midnight(beginning_of_week(bucket.date())));
    }
    naive_epoch(bucket)
}

fn bucket_end(bucket: NaiveDateTime, interval: Interval) -> i64 {
    let start = bucket_start(bucket, interval);
    match interval {
        Interval::Month => naive_epoch(midnight(add_months(bucket.date(), 1))) - 1,
        Interval::Week => start + 7 * 86_400 - 1,
        Interval::Day => start + 86_400 - 1,
        Interval::Hour => start + 3600 - 1,
        Interval::Minute => start + 59,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(now: i64) -> PeriodContext {
        PeriodContext { tz: Tz::UTC, now, relative_date: zoned_date(now, Tz::UTC), stats_start: None }
    }

    fn date(text: &str) -> NaiveDate {
        parse_date(text).unwrap()
    }

    // 2026-09-30T12:34:56Z, a Wednesday.
    const NOW: i64 = 1_790_771_696;

    #[test]
    fn periods() {
        for period in Period::ALL {
            assert_eq!(Period::parse(period.name()), Some(period));
        }
        assert_eq!(Period::parse("1w"), None);
    }

    #[test]
    fn ranges() {
        let c = ctx(NOW);
        let r = main_range(&DateRange::Period(Period::Days(7)), &c, None);
        assert_eq!(date_range(r, Tz::UTC), (date("2026-09-23"), date("2026-09-29")));
        let r = main_range(&DateRange::Period(Period::Month), &c, None);
        assert_eq!(date_range(r, Tz::UTC), (date("2026-09-01"), date("2026-09-30")));
        let r = main_range(&DateRange::Period(Period::Day), &c, None);
        assert_eq!(r.last, zoned_epoch(date("2026-09-30").and_hms_opt(12, 59, 59).unwrap(), Tz::UTC));
        let r = main_range(&DateRange::Period(Period::Months(6)), &c, None);
        assert_eq!(date_range(r, Tz::UTC), (date("2026-03-01"), date("2026-08-31")));
    }

    #[test]
    fn comparisons() {
        let c = ctx(NOW);
        let week = DateRange::Period(Period::Days(7));
        let main = main_range(&week, &c, Some(&Compare::PreviousPeriod));
        let prev = comparison_range(&week, main, &Compare::PreviousPeriod, false, &c);
        assert_eq!(date_range(prev, Tz::UTC), (date("2026-09-16"), date("2026-09-22")));
    }

    #[test]
    fn buckets() {
        let c = ctx(NOW);
        let labels = |interval: Interval, range| time_buckets(interval, range, Tz::UTC, NOW).into_iter().map(|b| interval.label(b)).collect::<Vec<_>>();
        let r = main_range(&DateRange::Period(Period::Days(7)), &c, None);
        assert_eq!(labels(Interval::Day, r).len(), 7);
        assert_eq!(labels(Interval::Week, r), ["2026-09-23", "2026-09-28"]);
        let hours = time_buckets(Interval::Hour, main_range(&DateRange::Period(Period::Day), &c, None), Tz::UTC, NOW);
        assert_eq!(hours.len(), 13);
        assert_eq!(Interval::Hour.label(hours[0]), "2026-09-30 00:00:00");
        assert_eq!(present_index(&hours, Interval::Hour, r, Tz::UTC, NOW), Some(12));
    }
}
