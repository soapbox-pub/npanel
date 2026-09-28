import type { Metric } from '@shared/analytics-query';

/** A count as Plausible shows it: exact below a thousand, then 1.2k, 3.4M. */
export function shortNumber(value: number): string {
  const abs = Math.abs(value);
  if (abs < 1000) return String(Math.round(value));

  for (const [size, suffix] of [
    [1e9, 'B'],
    [1e6, 'M'],
    [1e3, 'k'],
  ] as const) {
    if (abs >= size) {
      const scaled = value / size;
      return `${scaled >= 100 ? Math.round(scaled) : Number(scaled.toFixed(1))}${suffix}`;
    }
  }
  return String(value);
}

/** Seconds as `1h 2m`, `3m 4s` or `5s`. */
export function duration(seconds: number): string {
  const s = Math.round(seconds);
  const hours = Math.floor(s / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const rest = s % 60;

  if (hours) return `${hours}h ${minutes}m`;
  if (minutes) return `${minutes}m ${rest}s`;
  return `${rest}s`;
}

/** A metric's value, formatted for a card, a row or a tooltip. */
export function formatMetric(metric: Metric, value: number | null | undefined): string {
  if (value === null || value === undefined) return '-';

  switch (metric) {
    case 'bounce_rate':
    case 'scroll_depth':
      return `${Math.round(value)}%`;
    case 'percentage':
    case 'exit_rate':
      return `${Number(value.toFixed(1))}%`;
    case 'visit_duration':
    case 'time_on_page':
      return duration(value);
    case 'views_per_visit':
      return String(Number(value.toFixed(2)));
    default:
      return shortNumber(value);
  }
}

/** A metric's name, as a column or card heading. */
export const METRIC_LABELS: Record<Metric, string> = {
  visitors: 'Visitors',
  visits: 'Visits',
  pageviews: 'Pageviews',
  views_per_visit: 'Views per visit',
  bounce_rate: 'Bounce rate',
  visit_duration: 'Visit duration',
  time_on_page: 'Time on page',
  scroll_depth: 'Scroll depth',
  percentage: '%',
  exit_rate: 'Exit rate',
};

/** Whether going up is good news, which is what colours a change arrow. */
export function higherIsBetter(metric: Metric): boolean {
  return metric !== 'bounce_rate' && metric !== 'exit_rate';
}

/** A country's flag, made from its two letters. */
export function flag(code: string): string {
  if (!/^[A-Z]{2}$/.test(code)) return '';
  return String.fromCodePoint(...[...code].map((letter) => 0x1f1e6 + letter.charCodeAt(0) - 65));
}

/** What went wrong with a query, as a sentence for the page. */
export function errorText(error: unknown): string {
  return error instanceof Error ? error.message : 'Something went wrong.';
}
