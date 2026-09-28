import type { Metric } from '@shared/analytics-query';
import { ArrowDown, ArrowUp } from 'lucide-react';
import { useState } from 'react';
import { CartesianGrid, Line, LineChart, Tooltip, XAxis, YAxis } from 'recharts';

import { Card, CardContent } from '@/components/ui/card';
import { ChartContainer } from '@/components/ui/chart';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { useCurrentVisitors, useStats } from '@/hooks/useAnalytics';
import { METRIC_LABELS, errorText, formatMetric, higherIsBetter, shortNumber } from '@/lib/analytics/format';
import { type Interval, canCompare, defaultInterval, validIntervals } from '@/lib/analytics/periods';
import { type StatsResponse, statsQuery } from '@/lib/analytics/query';
import type { DashboardState } from '@/lib/analytics/state';
import { cn } from '@/lib/utils';

interface GraphProps {
  hostname: string;
  state: DashboardState;
}

const DEFAULT_METRICS: Metric[] = ['visitors', 'visits', 'pageviews', 'views_per_visit', 'bounce_rate', 'visit_duration'];
const PAGE_METRICS: Metric[] = ['visitors', 'visits', 'pageviews', 'bounce_rate', 'scroll_depth', 'time_on_page'];
const REALTIME_METRICS: Metric[] = ['visitors', 'pageviews'];

/** Metrics that can be drawn over time. Time on page and scroll depth are per page, not per bucket. */
const GRAPHABLE = new Set<Metric>(['visitors', 'visits', 'pageviews', 'views_per_visit', 'bounce_rate', 'visit_duration']);

const INTERVAL_LABELS: Record<Interval, string> = {
  minute: 'Minutes',
  hour: 'Hours',
  day: 'Days',
  week: 'Weeks',
  month: 'Months',
};

/**
 * The top of the dashboard: the headline numbers, and the graph of whichever
 * one is picked, the way Plausible lays them out.
 */
export function VisitorGraph({ hostname, state }: GraphProps) {
  const realtime = state.period === 'realtime';
  const pages = state.filters.some((filter) => filter.key === 'page');
  const metrics = realtime ? REALTIME_METRICS : pages ? PAGE_METRICS : DEFAULT_METRICS;

  const [picked, setPicked] = useState<Metric>('visitors');
  const selected = metrics.includes(picked) && GRAPHABLE.has(picked) ? picked : 'visitors';

  // Keyed on what the period allows, so a new period starts from its own default.
  const intervals = validIntervals(state);
  const [chosen, setChosen] = useState<{ for: string; interval: Interval } | undefined>();
  const interval =
    chosen && chosen.for === intervals.join() && intervals.includes(chosen.interval)
      ? chosen.interval
      : defaultInterval(state);

  // The cards always carry a change: against what the dashboard compares to,
  // or against the previous period when it compares to nothing.
  const cardState: DashboardState =
    state.comparison === 'off' && canCompare(state) ? { ...state, comparison: 'previous_period' } : state;

  const top = useStats(
    hostname,
    statsQuery(cardState, { metrics, dateRange: realtime ? 'realtime_30m' : undefined }),
    { live: realtime },
  );
  const graph = useStats(
    hostname,
    statsQuery(state, {
      metrics: [selected],
      dimensions: [`time:${realtime ? 'minute' : interval}`],
      timeLabels: true,
      dateRange: realtime ? 'realtime_30m' : undefined,
    }),
    { live: realtime },
  );

  const row = top.data?.results[0];

  return (
    <Card>
      <CardContent className="space-y-6 pt-6">
        <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
          {realtime && <CurrentVisitorsCard hostname={hostname} />}
          {metrics.map((metric, index) => (
            <StatCard
              key={metric}
              metric={metric}
              label={realtime ? `${METRIC_LABELS[metric]} (last 30 min)` : METRIC_LABELS[metric]}
              value={row?.metrics[index]}
              comparison={row?.comparison?.metrics[index]}
              change={row?.comparison?.change[index]}
              selected={metric === selected}
              selectable={GRAPHABLE.has(metric)}
              loading={top.isLoading}
              onSelect={() => setPicked(metric)}
            />
          ))}
        </div>

        {top.isError && <p className="text-sm text-destructive">{errorText(top.error)}</p>}

        <div className="space-y-2">
          {!realtime && intervals.length > 1 && (
            <div className="flex justify-end">
              <Select
                value={interval}
                onValueChange={(value) => setChosen({ for: intervals.join(), interval: value as Interval })}
              >
                <SelectTrigger size="sm" className="w-28">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {intervals.map((option) => (
                    <SelectItem key={option} value={option}>
                      {INTERVAL_LABELS[option]}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          )}
          {graph.data ? (
            <Graph data={graph.data} metric={selected} interval={realtime ? 'minute' : interval} />
          ) : (
            <Skeleton className="h-72 w-full" />
          )}
        </div>
      </CardContent>
    </Card>
  );
}

function CurrentVisitorsCard({ hostname }: { hostname: string }) {
  const current = useCurrentVisitors(hostname);

  return (
    <div className="rounded-md border p-3">
      <div className="flex items-center gap-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
        <span className="size-2 animate-pulse rounded-full bg-green-500" />
        Current visitors
      </div>
      <div className="mt-1 text-2xl font-semibold">{current.data ?? '-'}</div>
    </div>
  );
}

function StatCard(props: {
  metric: Metric;
  label: string;
  value: number | null | undefined;
  comparison: number | null | undefined;
  change: number | null | undefined;
  selected: boolean;
  selectable: boolean;
  loading: boolean;
  onSelect(): void;
}) {
  const { metric, value, change } = props;

  return (
    <button
      type="button"
      disabled={!props.selectable}
      onClick={props.onSelect}
      className={cn(
        'rounded-md border p-3 text-left transition-colors',
        props.selectable && 'hover:bg-muted/60',
        props.selected && 'border-primary bg-muted/40',
      )}
    >
      <div className="line-clamp-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">{props.label}</div>
      {props.loading ? (
        <Skeleton className="mt-2 h-7 w-16" />
      ) : (
        <div className="mt-1 flex items-baseline gap-2">
          <span className="text-2xl font-semibold">{formatMetric(metric, value)}</span>
          <Change metric={metric} change={change} />
        </div>
      )}
      {props.comparison !== undefined && props.comparison !== null && !props.loading && (
        <div className="mt-1 text-xs text-muted-foreground">
          previously {formatMetric(metric, props.comparison)}
        </div>
      )}
    </button>
  );
}

/** How a metric moved, coloured by whether that is good news for the site. */
export function Change({ metric, change }: { metric: Metric; change: number | null | undefined }) {
  if (change === null || change === undefined) return null;
  if (change === 0) return <span className="text-xs text-muted-foreground">〰 0%</span>;

  const good = change > 0 === higherIsBetter(metric);
  const Arrow = change > 0 ? ArrowUp : ArrowDown;
  const unit = metric === 'bounce_rate' || metric === 'exit_rate' ? 'pp' : '%';

  return (
    <span className={cn('flex items-center text-xs font-medium', good ? 'text-green-600' : 'text-red-600')}>
      <Arrow className="size-3" />
      {Math.abs(change)}
      {unit}
    </span>
  );
}

interface Point {
  label: string;
  value: number | null;
  /** The same value, drawn only across the buckets that aren't over yet. */
  partial: number | null;
  comparison: number | null;
  comparisonLabel?: string;
}

function Graph({ data, metric, interval }: { data: StatsResponse; metric: Metric; interval: Interval }) {
  const labels = data.meta.time_labels ?? [];
  const indices = data.meta.time_label_result_indices ?? [];
  const present = data.meta.present_index ?? null;
  const partial = new Set(data.meta.partial_time_labels ?? []);
  const empty = data.meta.empty_metrics?.[0] ?? 0;

  const comparisonLabels = data.meta.comparison_time_labels ?? [];
  const comparisonIndices = data.meta.comparison_time_label_result_indices ?? [];

  const points: Point[] = labels.map((label, index) => {
    const found = indices[index];
    // Nothing is drawn for the future, which is not the same as nothing happening.
    const future = present !== null && index > present;
    const value = future ? null : found === null || found === undefined ? empty : data.results[found].metrics[0];

    const comparisonIndex = comparisonIndices[index];
    const comparison =
      index < comparisonLabels.length
        ? comparisonIndex === null || comparisonIndex === undefined
          ? empty
          : (data.comparison_results?.[comparisonIndex]?.metrics[0] ?? empty)
        : null;

    return { label, value, partial: null, comparison, comparisonLabel: comparisonLabels[index] };
  });

  // A bucket that isn't over is drawn dashed, joined to the whole ones beside it.
  for (let index = 0; index < points.length; index++) {
    const point = points[index];
    if (!partial.has(point.label) || point.value === null) continue;

    point.partial = point.value;
    point.value = null;
    for (const neighbour of [points[index - 1], points[index + 1]]) {
      if (neighbour && !partial.has(neighbour.label) && neighbour.value !== null) neighbour.partial = neighbour.value;
    }
  }

  const comparing = comparisonLabels.length > 0;

  return (
    <ChartContainer
      config={{ value: { label: METRIC_LABELS[metric], color: 'var(--chart-1)' } }}
      className="aspect-auto h-72 w-full"
    >
      <LineChart data={points} margin={{ left: 4, right: 12, top: 8 }}>
        <CartesianGrid vertical={false} />
        <XAxis
          dataKey="label"
          tickLine={false}
          axisLine={false}
          minTickGap={24}
          tickFormatter={(label: string) => tickLabel(label, interval)}
        />
        <YAxis
          width={48}
          tickLine={false}
          axisLine={false}
          allowDecimals={metric === 'views_per_visit'}
          tickFormatter={(value: number) => (metric === 'visit_duration' ? formatMetric(metric, value) : shortNumber(value))}
        />
        <Tooltip
          content={({ active, payload }) => {
            const point = payload?.[0]?.payload as Point | undefined;
            if (!active || !point) return null;
            return (
              <div className="rounded-md border bg-background p-2 text-xs shadow-md">
                <div className="font-medium">{tooltipLabel(point.label, interval)}</div>
                <div>
                  {METRIC_LABELS[metric]}: {formatMetric(metric, point.value ?? point.partial)}
                </div>
                {comparing && point.comparisonLabel && (
                  <div className="text-muted-foreground">
                    {tooltipLabel(point.comparisonLabel, interval)}: {formatMetric(metric, point.comparison)}
                  </div>
                )}
              </div>
            );
          }}
        />
        {comparing && (
          <Line
            dataKey="comparison"
            stroke="var(--muted-foreground)"
            strokeOpacity={0.4}
            strokeWidth={2}
            dot={false}
            isAnimationActive={false}
          />
        )}
        <Line dataKey="value" stroke="var(--color-value)" strokeWidth={2} dot={false} isAnimationActive={false} />
        <Line
          dataKey="partial"
          stroke="var(--color-value)"
          strokeWidth={2}
          strokeDasharray="4 4"
          dot={false}
          isAnimationActive={false}
        />
      </LineChart>
    </ChartContainer>
  );
}

function tickLabel(label: string, interval: Interval): string {
  const date = new Date(label.includes(' ') ? `${label.replace(' ', 'T')}Z` : `${label}T00:00:00Z`);
  switch (interval) {
    case 'minute':
      return date.toLocaleTimeString(undefined, { hour: 'numeric', minute: '2-digit', timeZone: 'UTC' });
    case 'hour':
      return date.toLocaleTimeString(undefined, { hour: 'numeric', timeZone: 'UTC' });
    case 'month':
      return date.toLocaleDateString(undefined, { month: 'short', year: 'numeric', timeZone: 'UTC' });
    default:
      return date.toLocaleDateString(undefined, { month: 'short', day: 'numeric', timeZone: 'UTC' });
  }
}

function tooltipLabel(label: string, interval: Interval): string {
  const date = new Date(label.includes(' ') ? `${label.replace(' ', 'T')}Z` : `${label}T00:00:00Z`);
  if (interval === 'minute' || interval === 'hour') {
    return date.toLocaleString(undefined, {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
      hour: 'numeric',
      minute: '2-digit',
      timeZone: 'UTC',
    });
  }
  if (interval === 'week') return `Week of ${tickLabel(label, 'day')}`;
  if (interval === 'month') return tickLabel(label, 'month');
  return date.toLocaleDateString(undefined, { weekday: 'short', month: 'short', day: 'numeric', year: 'numeric', timeZone: 'UTC' });
}
