import type { Dimension, FilterOperator } from '@shared/analytics-query';

/**
 * What the analytics page is looking at — period, comparison, filters — and
 * how it lives in the URL, so a view can be bookmarked or pasted to someone.
 *
 * Modelled on Plausible's dashboard state and its query string: `period`,
 * `date`, `from`/`to`, `comparison`, `compare_from`/`compare_to`,
 * `match_day_of_week`, a repeated `f` per filter and `l` for the labels of
 * location filters, whose values are codes.
 */

export type PeriodKey =
  | 'realtime'
  | 'day'
  | '24h'
  | '7d'
  | '28d'
  | '30d'
  | '91d'
  | 'month'
  | '6mo'
  | '12mo'
  | 'year'
  | 'all'
  | 'custom';

export const PERIOD_KEYS: readonly PeriodKey[] = [
  'realtime',
  'day',
  '24h',
  '7d',
  '28d',
  '30d',
  '91d',
  'month',
  '6mo',
  '12mo',
  'year',
  'all',
  'custom',
];

export type ComparisonMode = 'off' | 'previous_period' | 'year_over_year' | 'custom';

/** A filter as the dashboard names it: without the `visit:` or `event:` prefix. */
export type FilterKey =
  | 'page'
  | 'entry_page'
  | 'exit_page'
  | 'source'
  | 'channel'
  | 'referrer'
  | 'utm_medium'
  | 'utm_source'
  | 'utm_campaign'
  | 'utm_content'
  | 'utm_term'
  | 'country'
  | 'region'
  | 'city'
  | 'screen'
  | 'browser'
  | 'browser_version'
  | 'os'
  | 'os_version';

export const FILTER_KEYS: readonly FilterKey[] = [
  'page',
  'entry_page',
  'exit_page',
  'source',
  'channel',
  'referrer',
  'utm_medium',
  'utm_source',
  'utm_campaign',
  'utm_content',
  'utm_term',
  'country',
  'region',
  'city',
  'screen',
  'browser',
  'browser_version',
  'os',
  'os_version',
];

export interface DashboardFilter {
  operator: FilterOperator;
  key: FilterKey;
  clauses: string[];
}

export interface DashboardState {
  period: PeriodKey;
  /** The day the period is relative to, or null for today. */
  date: string | null;
  from: string | null;
  to: string | null;
  comparison: ComparisonMode;
  compareFrom: string | null;
  compareTo: string | null;
  matchDayOfWeek: boolean;
  filters: DashboardFilter[];
  /** Display names for location filter values, which are codes. */
  labels: Record<string, string>;
}

export const DEFAULT_STATE: DashboardState = {
  period: '28d',
  date: null,
  from: null,
  to: null,
  comparison: 'off',
  compareFrom: null,
  compareTo: null,
  matchDayOfWeek: true,
  filters: [],
  labels: {},
};

const OPERATORS: readonly FilterOperator[] = ['is', 'is_not', 'contains', 'contains_not'];
const DATE = /^\d{4}-\d{2}-\d{2}$/;

/** Read the state from a query string, ignoring anything that doesn't parse. */
export function parseState(search: URLSearchParams): DashboardState {
  const period = search.get('period') as PeriodKey | null;
  const from = date(search.get('from'));
  const to = date(search.get('to'));
  const comparison = search.get('comparison') as ComparisonMode | null;

  const filters: DashboardFilter[] = [];
  for (const raw of search.getAll('f')) {
    const [operator, key, ...clauses] = raw.split(',').map(decodeURIComponent);
    if (!OPERATORS.includes(operator as FilterOperator)) continue;
    if (!FILTER_KEYS.includes(key as FilterKey) || !clauses.length) continue;
    filters.push({ operator: operator as FilterOperator, key: key as FilterKey, clauses });
  }

  const labels: Record<string, string> = {};
  for (const raw of search.getAll('l')) {
    const [value, label] = raw.split(',').map(decodeURIComponent);
    if (value && label) labels[value] = label;
  }

  const custom = period === 'custom' && from && to && from <= to;

  return {
    period: period && PERIOD_KEYS.includes(period) && (period !== 'custom' || custom) ? period : DEFAULT_STATE.period,
    date: date(search.get('date')),
    from: custom ? from : null,
    to: custom ? to : null,
    comparison: comparison && ['previous_period', 'year_over_year', 'custom'].includes(comparison) ? comparison : 'off',
    compareFrom: date(search.get('compare_from')),
    compareTo: date(search.get('compare_to')),
    matchDayOfWeek: search.get('match_day_of_week') !== 'false',
    filters,
    labels,
  };
}

/** The state as a query string, leaving out whatever is already the default. */
export function serializeState(state: DashboardState): URLSearchParams {
  const search = new URLSearchParams();

  if (state.period !== DEFAULT_STATE.period) search.set('period', state.period);
  if (state.date) search.set('date', state.date);
  if (state.period === 'custom' && state.from && state.to) {
    search.set('from', state.from);
    search.set('to', state.to);
  }
  if (state.comparison !== 'off') search.set('comparison', state.comparison);
  if (state.comparison === 'custom' && state.compareFrom && state.compareTo) {
    search.set('compare_from', state.compareFrom);
    search.set('compare_to', state.compareTo);
  }
  if (!state.matchDayOfWeek) search.set('match_day_of_week', 'false');

  for (const filter of state.filters) {
    search.append('f', [filter.operator, filter.key, ...filter.clauses].map(encodeURIComponent).join(','));
  }
  for (const filter of state.filters) {
    for (const clause of filter.clauses) {
      const label = state.labels[clause];
      if (label) search.append('l', [clause, label].map(encodeURIComponent).join(','));
    }
  }

  return search;
}

/** The dimension a filter key means to the API. */
export function filterDimension(key: FilterKey): Dimension {
  if (key === 'page') return 'event:page';
  if (key === 'screen') return 'visit:device';
  return `visit:${key}` as Dimension;
}

/** The filters as the API takes them. */
export function apiFilters(filters: DashboardFilter[]): [FilterOperator, Dimension, string[]][] {
  return filters.map((filter) => [filter.operator, filterDimension(filter.key), filter.clauses]);
}

/** The state with one filter set: added, or replacing whatever filtered the same key. */
export function withFilter(
  state: DashboardState,
  filter: DashboardFilter,
  labels: Record<string, string> = {},
): DashboardState {
  return {
    ...state,
    filters: [...state.filters.filter((existing) => existing.key !== filter.key), filter],
    labels: { ...state.labels, ...labels },
  };
}

export function withoutFilter(state: DashboardState, key: FilterKey): DashboardState {
  return { ...state, filters: state.filters.filter((filter) => filter.key !== key) };
}

export function findFilter(state: DashboardState, key: FilterKey): DashboardFilter | undefined {
  return state.filters.find((filter) => filter.key === key);
}

function date(value: string | null): string | null {
  return value && DATE.test(value) ? value : null;
}
