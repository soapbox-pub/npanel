import type { DashboardState, PeriodKey } from './state';

export type Interval = 'minute' | 'hour' | 'day' | 'week' | 'month';

/** What each period is called in the picker, as Plausible's dashboard words it. */
export const PERIOD_LABELS: Record<Exclude<PeriodKey, 'custom'>, string> = {
  realtime: 'Realtime',
  day: 'Today',
  '24h': 'Last 24 hours',
  '7d': 'Last 7 days',
  '28d': 'Last 28 days',
  '30d': 'Last 30 days',
  '91d': 'Last 91 days',
  month: 'Month to date',
  '6mo': 'Last 6 months',
  '12mo': 'Last 12 months',
  year: 'Year to date',
  all: 'All time',
};

/** The picker's groups, in order. */
export const PERIOD_GROUPS: Exclude<PeriodKey, 'custom'>[][] = [
  ['day', '24h', 'realtime'],
  ['7d', '28d', '30d', '91d'],
  ['month', '6mo', '12mo', 'year'],
  ['all'],
];

/** The intervals a period can be graphed by, the first being the default. Plausible's `intervals.ts`. */
const INTERVALS: Record<Exclude<PeriodKey, 'custom' | 'all'>, Interval[]> = {
  realtime: ['minute'],
  day: ['hour', 'minute'],
  '24h': ['hour', 'minute'],
  '7d': ['day', 'hour'],
  '28d': ['day', 'week'],
  '30d': ['day', 'week'],
  '91d': ['day', 'week', 'month'],
  month: ['day', 'week'],
  '6mo': ['month', 'day', 'week'],
  '12mo': ['month', 'day', 'week'],
  year: ['month', 'day', 'week'],
};

/**
 * The intervals the graph may use for a state. For a custom range and all
 * time they depend on how long the range is: hours under a day, days under a
 * month, weeks under six, months beyond.
 */
export function validIntervals(state: DashboardState, allTimeDays?: number): Interval[] {
  if (state.period !== 'custom' && state.period !== 'all') return INTERVALS[state.period];

  const days =
    state.period === 'custom' && state.from && state.to
      ? (Date.parse(state.to) - Date.parse(state.from)) / 86_400_000 + 1
      : (allTimeDays ?? 365);

  if (days <= 1) return ['hour', 'day'];
  if (days < 31) return ['day', 'week'];
  if (days < 183) return ['week', 'day', 'month'];
  return ['month', 'day', 'week'];
}

export function defaultInterval(state: DashboardState, allTimeDays?: number): Interval {
  return validIntervals(state, allTimeDays)[0];
}

/** The `date_range` a state asks the API for. */
export function apiDateRange(state: DashboardState): string | [string, string] {
  if (state.period === 'custom' && state.from && state.to) return [state.from, state.to];
  // The live view's graph and totals cover the last half hour; its lists, the last five minutes.
  return state.period === 'custom' ? '28d' : state.period;
}

/** Comparisons have nothing to measure against in these. */
export function canCompare(state: DashboardState): boolean {
  return state.period !== 'realtime' && state.period !== 'all';
}

/** The period's name, as the picker's button shows it. */
export function periodLabel(state: DashboardState): string {
  if (state.period === 'custom' && state.from && state.to) {
    return state.from === state.to ? formatDay(state.from) : `${formatDay(state.from)} – ${formatDay(state.to)}`;
  }
  if (state.period === 'custom') return 'Custom range';
  if (state.date && (state.period === 'day' || state.period === 'month' || state.period === 'year')) {
    if (state.period === 'day') return formatDay(state.date);
    if (state.period === 'month') return formatMonth(state.date);
    return state.date.slice(0, 4);
  }
  return PERIOD_LABELS[state.period];
}

export function formatDay(date: string): string {
  return new Date(`${date}T00:00:00Z`).toLocaleDateString(undefined, {
    day: 'numeric',
    month: 'short',
    year: 'numeric',
    timeZone: 'UTC',
  });
}

function formatMonth(date: string): string {
  return new Date(`${date}T00:00:00Z`).toLocaleDateString(undefined, {
    month: 'long',
    year: 'numeric',
    timeZone: 'UTC',
  });
}

/**
 * Step a calendar period by one: yesterday, last month, last year. Returns
 * the `date` to look from, or undefined when the period doesn't step.
 */
export function shiftPeriod(state: DashboardState, today: string, direction: -1 | 1): string | null | undefined {
  if (state.period !== 'day' && state.period !== 'month' && state.period !== 'year') return undefined;

  const current = new Date(`${state.date ?? today}T00:00:00Z`);
  if (state.period === 'day') current.setUTCDate(current.getUTCDate() + direction);
  if (state.period === 'month') current.setUTCMonth(current.getUTCMonth() + direction, 1);
  if (state.period === 'year') current.setUTCFullYear(current.getUTCFullYear() + direction, 0, 1);

  const next = current.toISOString().slice(0, 10);
  if (next > today) return undefined;
  const sameAsToday =
    state.period === 'day'
      ? next === today
      : state.period === 'month'
        ? next.slice(0, 7) === today.slice(0, 7)
        : next.slice(0, 4) === today.slice(0, 4);
  return sameAsToday ? null : next;
}
