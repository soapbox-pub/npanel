import type { Dimension, Metric } from '@shared/analytics-query';

import { apiDateRange, canCompare } from './periods';
import type { ApiFilter } from './reports';
import { type DashboardState, apiFilters } from './state';

/** The body of `POST /api/hosts/:hostname/stats/query`: Plausible's dashboard query. */
export interface StatsQueryBody {
  date_range: string | [string, string];
  relative_date: string | null;
  metrics: Metric[];
  dimensions: Dimension[];
  filters: ApiFilter[];
  order_by: [string, 'asc' | 'desc'][] | null;
  pagination: { limit: number; offset: number } | null;
  include: {
    time_labels: boolean;
    partial_time_labels: boolean;
    compare: string | [string, string] | null;
    compare_match_day_of_week: boolean;
    empty_metrics: boolean;
    present_index: boolean;
  };
}

export interface StatsRow {
  dimensions: (string | number)[];
  metrics: (number | null)[];
  comparison?: { dimensions: (string | number)[]; metrics: (number | null)[]; change: (number | null)[] };
}

export interface StatsResponse {
  results: StatsRow[];
  comparison_results?: (StatsRow & { change: (number | null)[] | null })[];
  meta: {
    time_labels?: string[];
    time_label_result_indices?: (number | null)[];
    comparison_time_labels?: string[];
    comparison_time_label_result_indices?: (number | null)[];
    partial_time_labels?: string[];
    comparison_partial_time_labels?: string[];
    present_index?: number | null;
    empty_metrics?: (number | null)[];
  };
  query: { date_range: [string, string]; comparison_date_range?: [string, string] };
}

export interface QueryOptions {
  metrics: Metric[];
  dimensions?: Dimension[];
  filters?: ApiFilter[];
  orderBy?: [string, 'asc' | 'desc'][];
  pagination?: { limit: number; offset: number };
  /** Whether to ask for a comparison, if the state has one on. */
  compare?: boolean;
  timeLabels?: boolean;
  /** Replaces the state's period: the live view's graph covers half an hour. */
  dateRange?: string;
}

/** A dashboard query for the state, as Plausible's `createStatsQuery` builds one. */
export function statsQuery(state: DashboardState, options: QueryOptions): StatsQueryBody {
  const comparing = options.compare !== false && canCompare(state) && state.comparison !== 'off';

  return {
    date_range: options.dateRange ?? apiDateRange(state),
    relative_date: state.date,
    metrics: options.metrics,
    dimensions: options.dimensions ?? [],
    filters: [...apiFilters(state.filters), ...(options.filters ?? [])],
    order_by: options.orderBy ?? null,
    pagination: options.pagination ?? null,
    include: {
      time_labels: options.timeLabels ?? false,
      partial_time_labels: options.timeLabels ?? false,
      compare: comparing ? comparison(state) : null,
      compare_match_day_of_week: state.matchDayOfWeek,
      empty_metrics: options.timeLabels ?? false,
      present_index: options.timeLabels ?? false,
    },
  };
}

function comparison(state: DashboardState): string | [string, string] | null {
  if (state.comparison === 'custom') {
    return state.compareFrom && state.compareTo ? [state.compareFrom, state.compareTo] : null;
  }
  return state.comparison === 'off' ? null : state.comparison;
}

/** The legacy query string the suggestions endpoint takes, for the same state. */
export function suggestionParams(state: DashboardState, filters: ApiFilter[], q: string): URLSearchParams {
  const params = new URLSearchParams();
  const range = apiDateRange(state);

  if (Array.isArray(range)) {
    params.set('period', 'custom');
    params.set('from', range[0]);
    params.set('to', range[1]);
  } else {
    params.set('period', range);
  }
  if (state.date) params.set('date', state.date);
  params.set('filters', JSON.stringify(filters));
  params.set('q', q);

  return params;
}
