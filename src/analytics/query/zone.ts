/**
 * Calendar arithmetic in a time zone, with nothing but `Intl`.
 *
 * Dates are ISO `YYYY-MM-DD` strings throughout, which is how Plausible labels
 * them and how SQLite prints them, so a date computed here and a date grouped
 * by in SQL compare as equal strings. Instants are epoch seconds.
 */

export type CivilDate = string;

/** A moment as a wall clock in some zone shows it. */
export interface WallClock {
  date: CivilDate;
  hour: number;
  minute: number;
  second: number;
}

const DAY = 86_400;

const formatters = new Map<string, Intl.DateTimeFormat>();

function formatter(timeZone: string): Intl.DateTimeFormat {
  let format = formatters.get(timeZone);
  if (!format) {
    format = new Intl.DateTimeFormat("en-US", {
      timeZone,
      hourCycle: "h23",
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
    formatters.set(timeZone, format);
  }
  return format;
}

/** What a wall clock in `timeZone` shows at `epoch`. */
export function wallClock(epoch: number, timeZone: string): WallClock {
  const parts: Record<string, string> = {};
  for (const part of formatter(timeZone).formatToParts(new Date(epoch * 1000))) parts[part.type] = part.value;

  return {
    date: `${parts.year}-${parts.month}-${parts.day}`,
    hour: Number(parts.hour),
    minute: Number(parts.minute),
    second: Number(parts.second),
  };
}

/** Seconds `timeZone` is ahead of UTC at `epoch`. */
export function offsetAt(epoch: number, timeZone: string): number {
  const clock = wallClock(Math.floor(epoch), timeZone);
  return civilEpoch(clock.date) + clock.hour * 3600 + clock.minute * 60 + clock.second - Math.floor(epoch);
}

/**
 * The instant a wall clock in `timeZone` shows the given time.
 *
 * Two passes, so a time on the far side of a daylight-saving change still
 * lands right. A time that never happens — inside a spring-forward gap —
 * lands an hour late, which for the start of a day is the first moment that
 * day has.
 */
export function zonedEpoch(date: CivilDate, timeZone: string, hour = 0, minute = 0, second = 0): number {
  const naive = civilEpoch(date) + hour * 3600 + minute * 60 + second;
  const first = naive - offsetAt(naive, timeZone);
  return naive - offsetAt(first, timeZone);
}

/** The local date `timeZone` is on at `epoch`. */
export function zonedDate(epoch: number, timeZone: string): CivilDate {
  return wallClock(epoch, timeZone).date;
}

/** The first and last second of a local day. */
export function dayBounds(date: CivilDate, timeZone: string): { first: number; last: number } {
  return { first: zonedEpoch(date, timeZone), last: zonedEpoch(addDays(date, 1), timeZone) - 1 };
}

/**
 * Where the zone's offset changes between two instants, as a list of
 * `[from, offset]` segments. Almost always one segment; two or three across a
 * daylight-saving change.
 *
 * Found by sampling every six hours — no zone changes offset twice in that
 * long — and narrowing each change down to the second.
 */
export function offsetSegments(from: number, to: number, timeZone: string): [number, number][] {
  const step = 6 * 3600;
  const segments: [number, number][] = [[from, offsetAt(from, timeZone)]];

  for (let at = from + step; at < to + step; at += step) {
    const sample = Math.min(at, to);
    const previous = segments[segments.length - 1][1];
    const offset = offsetAt(sample, timeZone);
    if (offset === previous) continue;

    let low = sample - step;
    let high = sample;
    while (high - low > 1) {
      const middle = Math.floor((low + high) / 2);
      if (offsetAt(middle, timeZone) === previous) low = middle;
      else high = middle;
    }
    segments.push([high, offset]);
  }

  return segments;
}

/** Midnight UTC of a date, which is what civil arithmetic is done in. */
export function civilEpoch(date: CivilDate): number {
  const [year, month, day] = date.split("-").map(Number);
  return Date.UTC(year, month - 1, day) / 1000;
}

function fromCivilEpoch(epoch: number): CivilDate {
  return new Date(epoch * 1000).toISOString().slice(0, 10);
}

export function addDays(date: CivilDate, days: number): CivilDate {
  return fromCivilEpoch(civilEpoch(date) + days * DAY);
}

/** Days from `from` to `to`. */
export function daysBetween(from: CivilDate, to: CivilDate): number {
  return Math.round((civilEpoch(to) - civilEpoch(from)) / DAY);
}

/** ISO weekday: 1 for Monday through 7 for Sunday. */
export function dayOfWeek(date: CivilDate): number {
  return new Date(civilEpoch(date) * 1000).getUTCDay() || 7;
}

export function beginningOfWeek(date: CivilDate): CivilDate {
  return addDays(date, 1 - dayOfWeek(date));
}

export function beginningOfMonth(date: CivilDate): CivilDate {
  return `${date.slice(0, 7)}-01`;
}

export function endOfMonth(date: CivilDate): CivilDate {
  return addDays(addMonths(beginningOfMonth(date), 1), -1);
}

/**
 * The same day `months` later, clamped to the end of a shorter month — which
 * is how Elixir's `Date.shift` does it, and so how Plausible's periods move.
 */
export function addMonths(date: CivilDate, months: number): CivilDate {
  const [year, month, day] = date.split("-").map(Number);
  const index = year * 12 + (month - 1) + months;
  const target = `${String(Math.floor(index / 12)).padStart(4, "0")}-${String((index % 12) + 1).padStart(2, "0")}-01`;
  const last = Number(endOfMonthOf(target).slice(8));
  return `${target.slice(0, 8)}${String(Math.min(day, last)).padStart(2, "0")}`;
}

export function addYears(date: CivilDate, years: number): CivilDate {
  return addMonths(date, years * 12);
}

function endOfMonthOf(first: CivilDate): CivilDate {
  const [year, month] = first.split("-").map(Number);
  return fromCivilEpoch(Date.UTC(year, month, 0) / 1000);
}

/** Whether a string is a real `YYYY-MM-DD` date. */
export function isCivilDate(value: unknown): value is CivilDate {
  return typeof value === "string" && /^\d{4}-\d{2}-\d{2}$/.test(value) && fromCivilEpoch(civilEpoch(value)) === value;
}

/**
 * An instant as ISO 8601 with the zone's offset, the way Plausible reports a
 * query's date range: `2026-08-31T00:00:00+02:00`, or `Z` when the zone is
 * UTC itself.
 */
export function isoInZone(epoch: number, timeZone: string): string {
  const clock = wallClock(epoch, timeZone);
  const offset = offsetAt(epoch, timeZone);
  const time = [clock.hour, clock.minute, clock.second].map((part) => String(part).padStart(2, "0")).join(":");

  if (timeZone === "UTC" || timeZone === "Etc/UTC") return `${clock.date}T${time}Z`;

  const sign = offset < 0 ? "-" : "+";
  const minutes = Math.abs(offset) / 60;
  const hh = String(Math.floor(minutes / 60)).padStart(2, "0");
  const mm = String(minutes % 60).padStart(2, "0");
  return `${clock.date}T${time}${sign}${hh}:${mm}`;
}
