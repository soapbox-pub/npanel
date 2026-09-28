import type { Metric } from '@shared/analytics-query';
import { ArrowDown, ArrowUp, Search } from 'lucide-react';
import { useState } from 'react';

import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { useStatsPages } from '@/hooks/useAnalytics';
import { useDebounce } from '@/hooks/useDebounce';
import alpha3 from '@/lib/analytics/alpha3.json';
import { METRIC_LABELS, errorText, flag, formatMetric } from '@/lib/analytics/format';
import { statsQuery } from '@/lib/analytics/query';
import type { ApiFilter, ReportList } from '@/lib/analytics/reports';
import { type DashboardState, withFilter } from '@/lib/analytics/state';

const COUNTRY_NAMES = new Intl.DisplayNames(['en'], { type: 'region' });

/**
 * A report in full: every row, more metrics, sortable by any column and
 * searchable, a hundred rows at a time.
 */
export function DetailsDialog({
  hostname,
  state,
  list,
  onChange,
  onClose,
}: {
  hostname: string;
  state: DashboardState;
  list: ReportList;
  onChange(state: DashboardState): void;
  onClose(): void;
}) {
  const [order, setOrder] = useState<[Metric, 'asc' | 'desc']>(['visitors', 'desc']);
  const [search, setSearch] = useState('');
  const query = useDebounce(search.trim(), 300);

  const metrics = state.period === 'realtime' ? (['visitors'] as Metric[]) : list.detailsMetrics;
  const pages = useStatsPages(
    hostname,
    statsQuery(state, {
      metrics,
      dimensions: list.dimensions,
      filters: [...list.alwaysOn, ...searchFilter(list, query)],
      orderBy: [order, [list.dimensions[list.labelIndex], 'asc']],
      compare: false,
    }),
  );

  const rows = pages.data?.pages.flatMap((page) => page.results) ?? [];

  const sortBy = (metric: Metric) =>
    setOrder(([current, direction]) => [metric, current === metric && direction === 'desc' ? 'asc' : 'desc']);

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[85vh] overflow-hidden sm:max-w-4xl">
        <DialogHeader>
          <DialogTitle>{list.tab}</DialogTitle>
          <DialogDescription className="sr-only">Every {list.title.toLowerCase()}, with all its metrics.</DialogDescription>
        </DialogHeader>

        <div className="relative">
          <Search className="absolute top-2.5 left-2.5 size-4 text-muted-foreground" />
          <Input
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder={`Search ${list.title.toLowerCase()}s`}
            className="pl-8"
          />
        </div>

        <div className="max-h-[60vh] overflow-y-auto">
          {pages.isLoading ? (
            <Skeleton className="h-64 w-full" />
          ) : pages.isError ? (
            <p className="py-8 text-center text-sm text-destructive">{errorText(pages.error)}</p>
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>{list.title}</TableHead>
                  {metrics.map((metric) => (
                    <TableHead key={metric} className="text-right">
                      <button type="button" className="inline-flex items-center gap-1" onClick={() => sortBy(metric)}>
                        {METRIC_LABELS[metric]}
                        {order[0] === metric &&
                          (order[1] === 'desc' ? <ArrowDown className="size-3" /> : <ArrowUp className="size-3" />)}
                      </button>
                    </TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {rows.map((row) => {
                  const label = String(row.dimensions[list.labelIndex]);
                  const value = String(row.dimensions[list.valueIndex]);
                  const country = list.countryIndex === undefined ? '' : String(row.dimensions[list.countryIndex]);

                  return (
                    <TableRow key={row.dimensions.join('\u0000')}>
                      <TableCell className="max-w-80 truncate">
                        <button
                          type="button"
                          className="truncate text-left hover:underline"
                          onClick={() => {
                            onChange(
                              withFilter(
                                state,
                                { operator: 'is', key: list.filterKey, clauses: [value] },
                                label !== value ? { [value]: label } : {},
                              ),
                            );
                            onClose();
                          }}
                        >
                          {country && `${flag(country)} `}
                          {label}
                        </button>
                      </TableCell>
                      {metrics.map((metric, index) => (
                        <TableCell key={metric} className="text-right tabular-nums">
                          {formatMetric(metric, row.metrics[index])}
                        </TableCell>
                      ))}
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          )}
          {!pages.isLoading && !rows.length && (
            <p className="py-8 text-center text-sm text-muted-foreground">Nothing matches.</p>
          )}
        </div>

        {pages.hasNextPage && (
          <div className="flex justify-center">
            <Button variant="outline" size="sm" disabled={pages.isFetchingNextPage} onClick={() => pages.fetchNextPage()}>
              {pages.isFetchingNextPage ? 'Loading…' : 'Load more'}
            </Button>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

/**
 * The search box as a filter on the list's own names.
 *
 * Countries are named by the browser and stored by code, so a search for one
 * becomes a filter for the codes whose names match.
 */
function searchFilter(list: ReportList, query: string): ApiFilter[] {
  if (!query) return [];

  if (list.id === 'countries') {
    const needle = query.toLowerCase();
    const codes = Object.keys(alpha3).filter((code) => COUNTRY_NAMES.of(code)?.toLowerCase().includes(needle));
    return [['is', 'visit:country', codes.length ? codes : ['']]];
  }

  return [['contains', list.dimensions[list.labelIndex], [query], { case_sensitive: false }]];
}
