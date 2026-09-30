//! The periods Plausible's dashboard offers, and how each turns into a range
//! of time — a port of `Stats.QueryPeriod`, `Stats.Comparisons` and
//! `Stats.Time`, down to their quirks, so that the same query asks the same
//! question of npanel as of Plausible.

use chrono_tz::Tz;

use super::zone::*;

pub const PERIODS: [&str; 13] =
    ["realtime", "realtime_30m", "day", "24h", "7d", "28d", "30d", "91d", "month", "6mo", "12mo", "year", "all"];

/// A named period, or a custom one from one date to another, inclusive.
#[derive(Clone, Debug, PartialEq)]
pub enum DateRange {
    Period(String),
    Custom(CivilDate, CivilDate),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Compare {
    PreviousPeriod,
    YearOverYear,
    Custom(CivilDate, CivilDate),
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
    pub fn parse(name: &str) -> Option<Interval> {
        Some(match name {
            "minute" => Interval::Minute,
            "hour" => Interval::Hour,
            "day" => Interval::Day,
            "week" => Interval::Week,
            "month" => Interval::Month,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Interval::Minute => "minute",
            Interval::Hour => "hour",
            Interval::Day => "day",
            Interval::Week => "week",
            Interval::Month => "month",
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
    pub relative_date: CivilDate,
    /// The first day the site has data, which is where `all` begins.
    pub stats_start: Option<CivilDate>,
}

/// From the start of one local day to the end of another.
fn dates(first: &str, last: &str, tz: Tz) -> TimeRange {
    TimeRange { first: day_bounds(first, tz).0, last: day_bounds(last, tz).1 }
}

/// The leading number of a period name, as `Number.parseInt` reads it.
fn leading_number(name: &str) -> i64 {
    name.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0)
}

/// The time a query covers, trimmed the way the dashboard asks for: an
/// in-progress month or year ends today, and today ends at the end of the
/// current hour unless it is being compared.
pub fn main_range(input: &DateRange, ctx: &PeriodContext, compare: Option<&Compare>) -> TimeRange {
    let (tz, now, relative) = (ctx.tz, ctx.now, ctx.relative_date.as_str());
    let today = zoned_date(now, tz);

    let period = match input {
        DateRange::Custom(first, last) => return dates(first, last, tz),
        DateRange::Period(period) => period.as_str(),
    };

    match period {
        "realtime" => TimeRange { first: now - 5 * 60, last: now + 5 },
        "realtime_30m" => TimeRange { first: now - 30 * 60, last: now + 5 },
        "24h" => TimeRange { first: now - 24 * 3600, last: now },
        "day" => {
            if relative != today {
                return dates(relative, relative, tz);
            }
            if compare.is_some() {
                return TimeRange { first: day_bounds(&today, tz).0, last: now };
            }
            let clock = wall_clock(now, tz);
            TimeRange { first: day_bounds(&today, tz).0, last: zoned_epoch(&today, tz, clock.hour, 59, 59) }
        }
        "7d" | "28d" | "30d" | "91d" => {
            let n = leading_number(period);
            dates(&add_days(relative, -n), &add_days(relative, -1), tz)
        }
        "6mo" | "12mo" => {
            let n = leading_number(period);
            dates(&beginning_of_month(&add_months(relative, -n)), &end_of_month(&add_months(relative, -1)), tz)
        }
        "month" => {
            let first = beginning_of_month(relative);
            let last = if first == beginning_of_month(&today) { today.clone() } else { end_of_month(relative) };
            dates(&first, &last, tz)
        }
        "year" => {
            let first = format!("{}-01-01", &relative[..4]);
            let last = if first[..4] == today[..4] { today.clone() } else { format!("{}-12-31", &relative[..4]) };
            dates(&first, &last, tz)
        }
        _ => dates(ctx.stats_start.as_deref().unwrap_or(relative), relative, tz),
    }
}

/// The range a query is compared against.
///
/// Today and the last 24 hours compare instant for instant — the same hours a
/// day or a week earlier. Everything else compares whole days, measured from
/// the part of the range that has already happened.
pub fn comparison_range(input: &DateRange, main: TimeRange, compare: &Compare, match_day_of_week: bool, ctx: &PeriodContext) -> TimeRange {
    let (tz, now) = (ctx.tz, ctx.now);

    if let Compare::Custom(first, last) = compare {
        return dates(first, last, tz);
    }

    let today = zoned_date(now, tz);
    let period = match input {
        DateRange::Period(p) => p.as_str(),
        DateRange::Custom(..) => "",
    };
    let by_instant = period == "24h" || (period == "day" && zoned_date(main.first, tz) == today);

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
        let days = days_between(&comparison_date, &shift_to_nearest(day_of_week(&source_date), &comparison_date, &source_date));
        return TimeRange { first: first + days * 86_400, last: last + days * 86_400 };
    }

    let (source_first, source_last) = date_range(main, tz);
    let source_last = if source_last > today {
        if today < source_first { source_first.clone() } else { today }
    } else {
        source_last
    };

    let (mut first, mut last) = if *compare == Compare::PreviousPeriod {
        let shift = days_between(&source_last, &source_first) - 1;
        (add_days(&source_first, shift), add_days(&source_last, shift))
    } else {
        let first = add_years(&source_first, -1);
        let last = add_days(&first, days_between(&source_first, &source_last));
        (first, last)
    };

    if match_day_of_week {
        let matched = shift_to_nearest(day_of_week(&source_first), &first, &source_first);
        let days = days_between(&first, &matched);
        first = matched;
        last = add_days(&last, days);
    }

    dates(&first, &last, tz)
}

/// The local dates a range starts and ends on.
pub fn date_range(range: TimeRange, tz: Tz) -> (CivilDate, CivilDate) {
    (zoned_date(range.first, tz), zoned_date(range.last, tz))
}

/// The buckets a time dimension divides a range into, as Plausible labels
/// them. Every bucket is listed, including the empty ones.
pub fn time_labels(interval: Interval, range: TimeRange, tz: Tz, now: i64) -> Vec<String> {
    let (first, last) = date_range(range, tz);
    let mut labels = Vec::new();

    match interval {
        Interval::Month => {
            let mut month = beginning_of_month(&first);
            while month <= last {
                labels.push(month.clone());
                month = add_months(&month, 1);
            }
        }
        Interval::Week => {
            let buckets = days_between(&beginning_of_week(&first), &last).div_euclid(7);
            for shift in 0..=buckets {
                labels.push(date_or_weekstart(&add_days(&first, shift * 7), &first, &last));
            }
        }
        Interval::Day => {
            let mut day = first.clone();
            while day <= last {
                labels.push(day.clone());
                day = add_days(&day, 1);
            }
        }
        Interval::Hour => {
            let clock = wall_clock(range.first, tz);
            let from = zoned_epoch(&clock.date, tz, clock.hour, 0, 0);
            let naive = civil_epoch(&clock.date) + clock.hour * 3600;
            let buckets = (range.last - from).div_euclid(3600);
            for step in 0..=buckets {
                labels.push(hour_label(naive + step * 3600));
            }
        }
        Interval::Minute => {
            let current_minute = now - now.rem_euclid(60);
            let mut at = range.first - range.first.rem_euclid(60);
            while at < range.last && at < current_minute {
                let clock = wall_clock(at, tz);
                labels.push(format!("{} {:02}:{:02}:00", clock.date, clock.hour, clock.minute));
                at += 60;
            }
        }
    }

    labels
}

/// The first and last bucket, when either reaches outside what the range has
/// covered so far. The graph draws those dashed.
pub fn partial_time_labels(labels: &[String], interval: Interval, range: TimeRange, tz: Tz, now: i64) -> Vec<String> {
    let (Some(first), Some(last)) = (labels.first(), labels.last()) else {
        return Vec::new();
    };
    let start = naive_epoch(range.first, tz);
    let cutoff = naive_epoch(now, tz).min(naive_epoch(range.last, tz));

    let mut partial = Vec::new();
    if start > bucket_start(first, interval) {
        partial.push(first.clone());
    }
    if bucket_end(last, interval) > cutoff && !partial.contains(last) {
        partial.push(last.clone());
    }
    partial
}

/// Which bucket now falls in, or `None` if none of them.
pub fn present_index(labels: &[String], interval: Interval, range: TimeRange, tz: Tz, now: i64) -> Option<usize> {
    let clock = wall_clock(now, tz);
    let label = match interval {
        Interval::Month => beginning_of_month(&clock.date),
        Interval::Week => {
            let (first, last) = date_range(range, tz);
            date_or_weekstart(&clock.date, &first, &last)
        }
        Interval::Day => clock.date.clone(),
        Interval::Hour => format!("{} {:02}:00:00", clock.date, clock.hour),
        Interval::Minute => format!("{} {:02}:{:02}:00", clock.date, clock.hour, clock.minute),
    };
    labels.iter().position(|l| *l == label)
}

/// Each hour or minute bucket as the span of instants it covers, for counting
/// a visit in every bucket it was active in.
pub fn bucket_spans(labels: &[String], interval: Interval, tz: Tz) -> Vec<(String, i64, i64)> {
    let length = if interval == Interval::Hour { 3600 } else { 60 };
    labels
        .iter()
        .map(|label| {
            let (date, time) = label.split_once(' ').unwrap_or((label, "00:00:00"));
            let mut parts = time.split(':').map(|p| p.parse::<i64>().unwrap_or(0));
            let (hour, minute) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
            let lo = zoned_epoch(date, tz, hour, minute, 0);
            (label.clone(), lo, lo + length)
        })
        .collect()
}

/// A week's label: its Monday, unless that falls outside the range, then the
/// date itself.
pub fn date_or_weekstart(date: &str, first: &str, last: &str) -> CivilDate {
    let weekstart = beginning_of_week(date);
    if weekstart.as_str() >= first && weekstart.as_str() <= last {
        weekstart
    } else {
        date.to_string()
    }
}

/// The same wall-clock time a year earlier.
fn shift_year(epoch: i64, tz: Tz) -> i64 {
    let clock = wall_clock(epoch, tz);
    zoned_epoch(&add_years(&clock.date, -1), tz, clock.hour, clock.minute, clock.second)
}

/// The date near `date` on the given weekday, never `reject` — Plausible's
/// `shift_to_nearest`, which tries the next such day before the previous one.
fn shift_to_nearest(weekday: i64, date: &str, reject: &str) -> CivilDate {
    if day_of_week(date) == weekday {
        return date.to_string();
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

/// Local wall-clock time as if it were UTC, for comparing against labels.
fn naive_epoch(epoch: i64, tz: Tz) -> i64 {
    let clock = wall_clock(epoch, tz);
    civil_epoch(&clock.date) + clock.hour * 3600 + clock.minute * 60 + clock.second
}

fn bucket_start(label: &str, interval: Interval) -> i64 {
    let (date, time) = match label.split_once(' ') {
        Some((date, time)) => (date, Some(time)),
        None => (label, None),
    };
    if interval == Interval::Week {
        return civil_epoch(&beginning_of_week(date));
    }
    let Some(time) = time else {
        return civil_epoch(date);
    };
    let parts: Vec<i64> = time.split(':').map(|p| p.parse().unwrap_or(0)).collect();
    civil_epoch(date) + parts.first().unwrap_or(&0) * 3600 + parts.get(1).unwrap_or(&0) * 60 + parts.get(2).unwrap_or(&0)
}

fn bucket_end(label: &str, interval: Interval) -> i64 {
    let start = bucket_start(label, interval);
    match interval {
        Interval::Month => civil_epoch(&add_months(&label[..10], 1)) - 1,
        Interval::Week => start + 7 * 86_400 - 1,
        Interval::Day => start + 86_400 - 1,
        Interval::Hour => start + 3600 - 1,
        Interval::Minute => start + 59,
    }
}

/// An hour's label, from local wall-clock time counted as if it were UTC.
fn hour_label(naive: i64) -> String {
    let time = chrono::DateTime::from_timestamp(naive, 0).unwrap_or_default();
    time.format("%Y-%m-%d %H:00:00").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(now: i64) -> PeriodContext {
        PeriodContext { tz: Tz::UTC, now, relative_date: zoned_date(now, Tz::UTC), stats_start: None }
    }

    // 2026-09-30T12:34:56Z, a Wednesday.
    const NOW: i64 = 1_790_771_696;

    #[test]
    fn ranges() {
        let c = ctx(NOW);
        let r = main_range(&DateRange::Period("7d".into()), &c, None);
        assert_eq!(date_range(r, Tz::UTC), ("2026-09-23".into(), "2026-09-29".into()));
        let r = main_range(&DateRange::Period("month".into()), &c, None);
        assert_eq!(date_range(r, Tz::UTC), ("2026-09-01".into(), "2026-09-30".into()));
        let r = main_range(&DateRange::Period("day".into()), &c, None);
        assert_eq!(r.last, zoned_epoch("2026-09-30", Tz::UTC, 12, 59, 59));
        let r = main_range(&DateRange::Period("6mo".into()), &c, None);
        assert_eq!(date_range(r, Tz::UTC), ("2026-03-01".into(), "2026-08-31".into()));
    }

    #[test]
    fn comparisons() {
        let c = ctx(NOW);
        let main = main_range(&DateRange::Period("7d".into()), &c, Some(&Compare::PreviousPeriod));
        let prev = comparison_range(&DateRange::Period("7d".into()), main, &Compare::PreviousPeriod, false, &c);
        assert_eq!(date_range(prev, Tz::UTC), ("2026-09-16".into(), "2026-09-22".into()));
    }

    #[test]
    fn labels() {
        let c = ctx(NOW);
        let r = main_range(&DateRange::Period("7d".into()), &c, None);
        let days = time_labels(Interval::Day, r, Tz::UTC, NOW);
        assert_eq!(days.len(), 7);
        let weeks = time_labels(Interval::Week, r, Tz::UTC, NOW);
        assert_eq!(weeks, ["2026-09-23", "2026-09-28"]);
        let hours = time_labels(Interval::Hour, main_range(&DateRange::Period("day".into()), &c, None), Tz::UTC, NOW);
        assert_eq!(hours.len(), 13);
        assert_eq!(hours[0], "2026-09-30 00:00:00");
        assert_eq!(present_index(&hours, Interval::Hour, r, Tz::UTC, NOW), Some(12));
    }
}
