import {
  type CivilDate,
  addDays,
  addMonths,
  addYears,
  beginningOfMonth,
  beginningOfWeek,
  civilEpoch,
  dayBounds,
  dayOfWeek,
  daysBetween,
  endOfMonth,
  wallClock,
  zonedDate,
  zonedEpoch,
} from "./zone.ts";

/**
 * The periods Plausible's dashboard offers, and how each turns into a range
 * of time — a port of `Stats.QueryPeriod`, `Stats.Comparisons` and
 * `Stats.Time`, down to their quirks, so that the same query asks the same
 * question of npanel as of Plausible.
 */

export type Period =
  | "realtime"
  | "realtime_30m"
  | "day"
  | "24h"
  | "7d"
  | "28d"
  | "30d"
  | "91d"
  | "month"
  | "6mo"
  | "12mo"
  | "year"
  | "all";

export const PERIODS: readonly Period[] = [
  "realtime",
  "realtime_30m",
  "day",
  "24h",
  "7d",
  "28d",
  "30d",
  "91d",
  "month",
  "6mo",
  "12mo",
  "year",
  "all",
];

/** A named period, or a custom one from one date to another, inclusive. */
export type DateRangeInput = Period | [CivilDate, CivilDate];

export type Compare = "previous_period" | "year_over_year" | [CivilDate, CivilDate];

export type Interval = "minute" | "hour" | "day" | "week" | "month";

/** A span of time, as epoch seconds, both ends included. */
export interface TimeRange {
  first: number;
  last: number;
}

/** What a period is resolved against. */
export interface PeriodContext {
  timezone: string;
  /** Epoch seconds. */
  now: number;
  /** The day the dashboard is looking from; today unless it says otherwise. */
  relativeDate: CivilDate;
  /** The first day the site has data, which is where `all` begins. */
  statsStart: CivilDate | undefined;
}

/**
 * The time a query covers.
 *
 * Trimmed the way the dashboard asks for, so an in-progress month or year ends
 * today rather than at a future the graph would draw as a flat line, and today
 * ends at the end of the current hour unless it is being compared.
 */
export function mainRange(input: DateRangeInput, ctx: PeriodContext, compare: Compare | null): TimeRange {
  const { timezone, now, relativeDate } = ctx;
  const today = zonedDate(now, timezone);

  if (Array.isArray(input)) return dates(input[0], input[1], timezone);

  switch (input) {
    case "realtime":
      return { first: now - 5 * 60, last: now + 5 };
    case "realtime_30m":
      return { first: now - 30 * 60, last: now + 5 };
    case "24h":
      return { first: now - 24 * 3600, last: now };
    case "day": {
      if (relativeDate !== today) return dates(relativeDate, relativeDate, timezone);
      if (compare) return { first: dayBounds(today, timezone).first, last: now };

      const clock = wallClock(now, timezone);
      return { first: dayBounds(today, timezone).first, last: zonedEpoch(today, timezone, clock.hour, 59, 59) };
    }
    case "7d":
    case "28d":
    case "30d":
    case "91d": {
      const n = Number.parseInt(input, 10);
      return dates(addDays(relativeDate, -n), addDays(relativeDate, -1), timezone);
    }
    case "6mo":
    case "12mo": {
      const n = Number.parseInt(input, 10);
      return dates(beginningOfMonth(addMonths(relativeDate, -n)), endOfMonth(addMonths(relativeDate, -1)), timezone);
    }
    case "month": {
      const first = beginningOfMonth(relativeDate);
      const last = endOfMonth(relativeDate);
      return dates(first, first === beginningOfMonth(today) ? today : last, timezone);
    }
    case "year": {
      const first = `${relativeDate.slice(0, 4)}-01-01`;
      const last = `${relativeDate.slice(0, 4)}-12-31`;
      return dates(first, first.slice(0, 4) === today.slice(0, 4) ? today : last, timezone);
    }
    case "all":
      return dates(ctx.statsStart ?? relativeDate, relativeDate, timezone);
  }
}

/**
 * The range a query is compared against.
 *
 * Today and the last 24 hours compare instant for instant — the same hours a
 * day or a week earlier. Everything else compares whole days, measured from
 * the part of the range that has already happened.
 */
export function comparisonRange(
  input: DateRangeInput,
  main: TimeRange,
  compare: Compare,
  matchDayOfWeek: boolean,
  ctx: PeriodContext,
): TimeRange {
  const { timezone, now } = ctx;

  if (Array.isArray(compare)) return dates(compare[0], compare[1], timezone);

  const today = zonedDate(now, timezone);
  const byInstant = input === "24h" || (input === "day" && zonedDate(main.first, timezone) === today);

  if (byInstant) {
    if (compare === "previous_period") {
      const shift = matchDayOfWeek ? 7 * 86_400 : 86_400;
      return { first: main.first - shift, last: main.last - shift };
    }

    const first = shiftYear(main.first, timezone);
    const last = shiftYear(main.last, timezone);
    if (!matchDayOfWeek) return { first, last };

    const sourceDate = zonedDate(main.first, timezone);
    const comparisonDate = zonedDate(first, timezone);
    const days = daysBetween(comparisonDate, shiftToNearest(dayOfWeek(sourceDate), comparisonDate, sourceDate));
    return { first: first + days * 86_400, last: last + days * 86_400 };
  }

  const source = dateRange(main, timezone);
  const sourceLast = source.last > today ? (today < source.first ? source.first : today) : source.last;

  let first: CivilDate;
  let last: CivilDate;
  if (compare === "previous_period") {
    const shift = daysBetween(sourceLast, source.first) - 1;
    first = addDays(source.first, shift);
    last = addDays(sourceLast, shift);
  } else {
    first = addYears(source.first, -1);
    last = addDays(first, daysBetween(source.first, sourceLast));
  }

  if (matchDayOfWeek) {
    const matched = shiftToNearest(dayOfWeek(source.first), first, source.first);
    const days = daysBetween(first, matched);
    first = matched;
    last = addDays(last, days);
  }

  return dates(first, last, timezone);
}

/** The local dates a range starts and ends on. */
export function dateRange(range: TimeRange, timezone: string): { first: CivilDate; last: CivilDate } {
  return { first: zonedDate(range.first, timezone), last: zonedDate(range.last, timezone) };
}

/**
 * The buckets a time dimension divides a range into, as Plausible labels
 * them. Every bucket is listed, including the empty ones, which is what lets a
 * graph draw a zero rather than skip a day.
 */
export function timeLabels(interval: Interval, range: TimeRange, timezone: string, now: number): string[] {
  const days = dateRange(range, timezone);

  switch (interval) {
    case "month": {
      const labels: string[] = [];
      for (let month = beginningOfMonth(days.first); month <= days.last; month = addMonths(month, 1)) {
        labels.push(month);
      }
      return labels;
    }

    case "week": {
      const buckets = Math.floor(daysBetween(beginningOfWeek(days.first), days.last) / 7);
      const labels: string[] = [];
      for (let shift = 0; shift <= buckets; shift++) {
        labels.push(dateOrWeekstart(addDays(days.first, shift * 7), days));
      }
      return labels;
    }

    case "day": {
      const labels: string[] = [];
      for (let day = days.first; day <= days.last; day = addDays(day, 1)) labels.push(day);
      return labels;
    }

    case "hour": {
      const clock = wallClock(range.first, timezone);
      const from = zonedEpoch(clock.date, timezone, clock.hour);
      const naive = civilEpoch(clock.date) + clock.hour * 3600;
      const buckets = Math.floor((range.last - from) / 3600);

      return Array.from({ length: buckets + 1 }, (_, step) => hourLabel(naive + step * 3600));
    }

    case "minute": {
      const labels: string[] = [];
      const currentMinute = now - (now % 60);
      for (let at = range.first - (range.first % 60); at < range.last && at < currentMinute; at += 60) {
        const clock = wallClock(at, timezone);
        labels.push(`${clock.date} ${pad(clock.hour)}:${pad(clock.minute)}:00`);
      }
      return labels;
    }
  }
}

/**
 * The first and last bucket, when either reaches outside what the range has
 * covered so far — a week the range starts halfway through, or today, which
 * isn't over. The graph draws those dashed.
 */
export function partialTimeLabels(
  labels: string[],
  interval: Interval,
  range: TimeRange,
  timezone: string,
  now: number,
): string[] {
  if (!labels.length) return [];

  const start = naiveEpoch(range.first, timezone);
  const cutoff = Math.min(naiveEpoch(now, timezone), naiveEpoch(range.last, timezone));
  const first = labels[0];
  const last = labels[labels.length - 1];

  const partial = new Set<string>();
  if (start > bucketStart(first, interval)) partial.add(first);
  if (bucketEnd(last, interval) > cutoff) partial.add(last);

  return [...partial];
}

/** Which bucket now falls in, or null if none of them. */
export function presentIndex(labels: string[], interval: Interval, range: TimeRange, timezone: string, now: number) {
  const clock = wallClock(now, timezone);
  const label =
    interval === "month"
      ? beginningOfMonth(clock.date)
      : interval === "week"
        ? dateOrWeekstart(clock.date, dateRange(range, timezone))
        : interval === "day"
          ? clock.date
          : interval === "hour"
            ? `${clock.date} ${pad(clock.hour)}:00:00`
            : `${clock.date} ${pad(clock.hour)}:${pad(clock.minute)}:00`;

  const index = labels.indexOf(label);
  return index === -1 ? null : index;
}

/**
 * Each hour or minute bucket as the span of instants it covers, for counting a
 * visit in every bucket it was active in rather than only the one it began
 * in.
 */
export function bucketSpans(
  labels: string[],
  interval: "hour" | "minute",
  timezone: string,
): [string, number, number][] {
  const length = interval === "hour" ? 3600 : 60;

  return labels.map((label) => {
    const [date, time] = label.split(" ");
    const [hour, minute] = time.split(":").map(Number);
    const lo = zonedEpoch(date, timezone, hour, minute);
    return [label, lo, lo + length];
  });
}

/** A week's label: its Monday, unless that falls before the range, then the range's first day. */
export function dateOrWeekstart(date: CivilDate, range: { first: CivilDate; last: CivilDate }): CivilDate {
  const weekstart = beginningOfWeek(date);
  return weekstart >= range.first && weekstart <= range.last ? weekstart : date;
}

/** From the start of one local day to the end of another. */
function dates(first: CivilDate, last: CivilDate, timezone: string): TimeRange {
  return { first: dayBounds(first, timezone).first, last: dayBounds(last, timezone).last };
}

/** The same wall-clock time a year earlier. */
function shiftYear(epoch: number, timezone: string): number {
  const clock = wallClock(epoch, timezone);
  return zonedEpoch(addYears(clock.date, -1), timezone, clock.hour, clock.minute, clock.second);
}

/**
 * The date near `date` on the given weekday, never `reject` — Plausible's
 * `shift_to_nearest`, which tries the next such day before the previous one.
 */
function shiftToNearest(weekday: number, date: CivilDate, reject: CivilDate): CivilDate {
  if (dayOfWeek(date) === weekday) return date;

  let forward = weekday - dayOfWeek(date);
  if (forward <= 0) forward += 7;
  let back = dayOfWeek(date) - weekday;
  if (back <= 0) back += 7;

  const next = addDays(date, forward);
  return next !== reject ? next : addDays(date, -back);
}

/** Local wall-clock time as if it were UTC, for comparing against labels. */
function naiveEpoch(epoch: number, timezone: string): number {
  const clock = wallClock(epoch, timezone);
  return civilEpoch(clock.date) + clock.hour * 3600 + clock.minute * 60 + clock.second;
}

function bucketStart(label: string, interval: Interval): number {
  const [date, time] = label.split(" ");
  if (interval === "week") return civilEpoch(beginningOfWeek(date));
  if (!time) return civilEpoch(date);
  const [hour, minute, second] = time.split(":").map(Number);
  return civilEpoch(date) + hour * 3600 + minute * 60 + second;
}

function bucketEnd(label: string, interval: Interval): number {
  const start = bucketStart(label, interval);
  const date = label.slice(0, 10);

  switch (interval) {
    case "month":
      return civilEpoch(addMonths(date, 1)) - 1;
    case "week":
      return start + 7 * 86_400 - 1;
    case "day":
      return start + 86_400 - 1;
    case "hour":
      return start + 3600 - 1;
    case "minute":
      return start + 59;
  }
}

/** An hour's label, from local wall-clock time counted as if it were UTC. */
function hourLabel(naive: number): string {
  const iso = new Date(naive * 1000).toISOString();
  return `${iso.slice(0, 10)} ${iso.slice(11, 13)}:00:00`;
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}
