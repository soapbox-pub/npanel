import { ExternalLink, Maximize2 } from 'lucide-react';
import { Suspense, lazy, useState } from 'react';

import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader } from '@/components/ui/card';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { useStats } from '@/hooks/useAnalytics';
import { errorText, flag, formatMetric } from '@/lib/analytics/format';
import { statsQuery } from '@/lib/analytics/query';
import { REPORTS, type ReportList } from '@/lib/analytics/reports';
import { type DashboardState, type FilterKey, findFilter, withFilter } from '@/lib/analytics/state';
import { cn } from '@/lib/utils';

const WorldMap = lazy(() => import('./WorldMap'));

/** Rows a card shows before "Details". */
const INDEX_ROWS = 9;

interface CardProps {
  hostname: string;
  state: DashboardState;
  onChange(state: DashboardState): void;
  /** Open the details dialog for a list. */
  onDetails(list: string): void;
}

interface Tab {
  id: string;
  /** What the tab is called, when it isn't simply its list's name. */
  label?: string;
  /** The list it shows, or the map. */
  list: ReportList | 'map';
}

/**
 * The page a Top Pages row names, on its own site, or `undefined` when the
 * recorded path wouldn't stay there. Paths are whatever a sender reported, so
 * one beginning `.evil.com/` or `@evil.com/` would otherwise lead elsewhere.
 */
function pageUrl(hostname: string, path: string): string | undefined {
  if (!path.startsWith('/') || path.startsWith('//')) return undefined;
  try {
    const url = new URL(path, `https://${hostname}`);
    return url.host === hostname ? url.href : undefined;
  } catch {
    return undefined;
  }
}

/** The report a single-value `is` filter drills a list down into, if it has one. */
function drilldown(state: DashboardState, key: FilterKey, list: ReportList, into: ReportList): ReportList {
  const filter = findFilter(state, key);
  return filter?.operator === 'is' && filter.clauses.length === 1 ? into : list;
}

export function SourcesCard(props: CardProps) {
  const [campaign, setCampaign] = useState<keyof typeof REPORTS>('utm_campaign');
  const utm = ['utm_medium', 'utm_source', 'utm_campaign', 'utm_content', 'utm_term'];

  return (
    <ReportCard
      {...props}
      tabs={[
        { id: 'channels', list: REPORTS.channels },
        { id: 'sources', list: drilldown(props.state, 'source', REPORTS.sources, REPORTS.referrers) },
        { id: 'campaigns', label: 'Campaigns', list: REPORTS[campaign] },
      ]}
      extra={(active) =>
        active === 'campaigns' && (
          <Select value={campaign} onValueChange={(value) => setCampaign(value)}>
            <SelectTrigger size="sm" className="w-36">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {utm.map((id) => (
                <SelectItem key={id} value={id}>
                  {REPORTS[id].tab}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        )
      }
    />
  );
}

export function PagesCard(props: CardProps) {
  return (
    <ReportCard
      {...props}
      tabs={[
        { id: 'pages', list: REPORTS.pages },
        { id: 'entry_pages', list: REPORTS.entry_pages },
        { id: 'exit_pages', list: REPORTS.exit_pages },
      ]}
    />
  );
}

export function LocationsCard(props: CardProps) {
  return (
    <ReportCard
      {...props}
      tabs={[
        { id: 'map', label: 'Map', list: 'map' },
        { id: 'countries', list: REPORTS.countries },
        { id: 'regions', list: REPORTS.regions },
        { id: 'cities', list: REPORTS.cities },
      ]}
      footer={
        // DB-IP's licence asks for this wherever its data is shown.
        <a
          href="https://db-ip.com"
          target="_blank"
          rel="noreferrer noopener"
          className="text-xs text-muted-foreground hover:underline"
        >
          IP Geolocation by DB-IP
        </a>
      }
    />
  );
}

export function DevicesCard(props: CardProps) {
  return (
    <ReportCard
      {...props}
      tabs={[
        { id: 'browsers', list: drilldown(props.state, 'browser', REPORTS.browsers, REPORTS.browser_versions) },
        {
          id: 'os',
          list: drilldown(props.state, 'os', REPORTS.operating_systems, REPORTS.operating_system_versions),
        },
        { id: 'devices', list: REPORTS.screen_sizes },
      ]}
    />
  );
}

function ReportCard({
  tabs,
  extra,
  footer,
  ...props
}: CardProps & { tabs: Tab[]; extra?(active: string): React.ReactNode; footer?: React.ReactNode }) {
  const [active, setActive] = useState(tabs[0].id);
  const tab = tabs.find((candidate) => candidate.id === active) ?? tabs[0];

  return (
    <Card className="flex flex-col">
      <CardHeader className="flex flex-row flex-wrap items-center justify-between gap-2 space-y-0 pb-2">
        <div className="flex flex-wrap rounded-lg bg-foreground/[0.06] p-0.5 dark:bg-foreground/10">
          {tabs.map((candidate) => (
            <button
              key={candidate.id}
              type="button"
              aria-pressed={candidate.id === tab.id}
              onClick={() => setActive(candidate.id)}
              className={cn(
                'rounded-md px-2.5 py-1 text-xs font-medium transition-colors focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden',
                candidate.id === tab.id
                  ? 'bg-card text-foreground shadow-sm dark:bg-secondary'
                  : 'text-muted-foreground hover:text-foreground',
              )}
            >
              {candidate.label ?? (candidate.list === 'map' ? '' : candidate.list.tab)}
            </button>
          ))}
        </div>
        {extra?.(tab.id)}
      </CardHeader>
      <CardContent className="flex flex-1 flex-col">
        {tab.list === 'map' ? (
          <Suspense fallback={<Skeleton className="h-64 w-full" />}>
            <WorldMap {...props} />
          </Suspense>
        ) : (
          <ListView key={tab.list.id} list={tab.list} {...props} />
        )}
        {footer && <div className="mt-3">{footer}</div>}
      </CardContent>
    </Card>
  );
}

function ListView({ list, hostname, state, onChange, onDetails }: CardProps & { list: ReportList }) {
  const stats = useStats(
    hostname,
    statsQuery(state, {
      metrics: list.metrics,
      dimensions: list.dimensions,
      filters: list.alwaysOn,
      orderBy: [
        ['visitors', 'desc'],
        [list.dimensions[list.labelIndex], 'asc'],
      ],
      pagination: { limit: INDEX_ROWS, offset: 0 },
      compare: false,
    }),
    { live: state.period === 'realtime' },
  );

  if (stats.isLoading) return <Skeleton className="h-64 w-full" />;
  if (stats.isError) return <p className="py-8 text-center text-sm text-destructive">{errorText(stats.error)}</p>;

  const rows = stats.data?.results ?? [];
  if (!rows.length) return <p className="py-16 text-center text-sm text-muted-foreground">No data yet.</p>;

  const max = Math.max(...rows.map((row) => row.metrics[0] ?? 0), 1);

  return (
    <div className="flex flex-1 flex-col">
      <div className="mb-1 flex justify-between px-2 text-xs font-medium text-muted-foreground">
        <span>{list.title}</span>
        <span>{state.period === 'realtime' ? 'Current visitors' : 'Visitors'}</span>
      </div>
      <ul className="flex-1 space-y-1">
        {rows.map((row) => {
          const label = String(row.dimensions[list.labelIndex]);
          const value = String(row.dimensions[list.valueIndex]);
          const country = list.countryIndex === undefined ? '' : String(row.dimensions[list.countryIndex]);
          const visitors = row.metrics[0] ?? 0;

          return (
            <li key={row.dimensions.join('\u0000')} className="group flex items-center gap-2 text-sm">
              <button
                type="button"
                className="relative flex min-w-0 flex-1 items-center gap-2 overflow-hidden rounded-md px-2 py-1 text-left hover:bg-muted/60"
                onClick={() =>
                  onChange(
                    withFilter(
                      state,
                      { operator: 'is', key: list.filterKey, clauses: [value] },
                      label !== value ? { [value]: label } : {},
                    ),
                  )
                }
              >
                <span
                  className="absolute inset-y-0 left-0 rounded-md bg-primary/10"
                  style={{ width: `${(visitors / max) * 100}%` }}
                />
                <span className="relative truncate">
                  {country && `${flag(country)} `}
                  {label}
                </span>
              </button>
              {list.pages && pageUrl(hostname, label) && (
                <a
                  href={pageUrl(hostname, label)}
                  target="_blank"
                  rel="noreferrer noopener"
                  aria-label={`Open ${label}`}
                  className="text-muted-foreground opacity-0 group-hover:opacity-100"
                >
                  <ExternalLink className="size-3.5" />
                </a>
              )}
              <span className="w-12 text-right font-medium">{formatMetric('visitors', visitors)}</span>
              <span className="w-14 text-right text-xs text-muted-foreground">
                {formatMetric('percentage', row.metrics[1])}
              </span>
            </li>
          );
        })}
      </ul>
      <div className="mt-3 flex justify-center">
        <Button variant="ghost" size="sm" className="gap-1" onClick={() => onDetails(list.id)}>
          <Maximize2 className="size-3.5" />
          Details
        </Button>
      </div>
    </div>
  );
}
