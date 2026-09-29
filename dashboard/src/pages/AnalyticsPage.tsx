import { useSeoMeta } from '@unhead/react';
import { useParams, useSearchParams } from 'react-router-dom';

import { DetailsDialog } from '@/components/analytics/DetailsDialog';
import { Filters } from '@/components/analytics/Filters';
import { ComparisonPicker, PeriodPicker } from '@/components/analytics/PeriodPicker';
import { PageHeader } from '@/components/PageHeader';
import { SiteIcon } from '@/components/SiteIcon';
import { DevicesCard, LocationsCard, PagesCard, SourcesCard } from '@/components/analytics/Reports';
import { VisitorGraph } from '@/components/analytics/VisitorGraph';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { useCurrentVisitors } from '@/hooks/useAnalytics';
import { useDashboardState } from '@/hooks/useDashboardState';
import { useHost, useUpdateHost } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import { REPORTS } from '@/lib/analytics/reports';
import type { DashboardState } from '@/lib/analytics/state';

/**
 * A site's analytics, laid out as Plausible lays out a site's dashboard: the
 * period and filters, the headline numbers and their graph, then sources,
 * pages, locations and devices.
 *
 * Everything the page shows is in its URL, so a view can be bookmarked or
 * passed to someone who may see the site.
 */
export function AnalyticsPage() {
  const hostname = useParams().hostname ?? '';
  const host = useHost(hostname);
  const [state, setState] = useDashboardState();
  const [search, setSearch] = useSearchParams();

  useSeoMeta({ title: `${hostname} analytics — npanel` });

  const back = { to: `/hosts/${hostname}`, label: hostname };

  if (host.isLoading) return <Skeleton className="h-96 w-full rounded-2xl" />;
  if (host.isError || !host.data) {
    return (
      <div className="space-y-6">
        <PageHeader back={back} title="Site not found" />
        <p className="text-muted-foreground">
          {host.error instanceof Error ? host.error.message : 'There is no site by that name.'}
        </p>
      </div>
    );
  }

  const details = search.get('details');
  const list = details ? REPORTS[details] : undefined;

  const setDetails = (id: string | null) =>
    setSearch((current) => {
      const next = new URLSearchParams(current);
      if (id) next.set('details', id);
      else next.delete('details');
      return next;
    });

  const cardProps = { hostname, state, onChange: setState, onDetails: setDetails };

  return (
    <div className="space-y-6">
      <PageHeader
        back={back}
        icon={<SiteIcon hostname={hostname} src={host.data.preview.icon} className="size-10" />}
        title="Analytics"
        actions={<CurrentVisitors hostname={hostname} state={state} onChange={setState} />}
      />

      {!host.data.analytics && <CountingOff hostname={hostname} />}

      <div className="flex flex-wrap items-center gap-2">
        <PeriodPicker state={state} onChange={setState} today={today()} />
        <ComparisonPicker state={state} onChange={setState} today={today()} />
        <Filters hostname={hostname} state={state} onChange={setState} />
      </div>

      <VisitorGraph hostname={hostname} state={state} />

      <div className="grid gap-6 lg:grid-cols-2">
        <SourcesCard {...cardProps} />
        <PagesCard {...cardProps} />
        <LocationsCard {...cardProps} />
        <DevicesCard {...cardProps} />
      </div>

      {list && (
        <DetailsDialog
          hostname={hostname}
          state={state}
          list={list}
          onChange={setState}
          onClose={() => setDetails(null)}
        />
      )}
    </div>
  );
}

/** Who's on the site now; a click shows them live. */
function CurrentVisitors({
  hostname,
  state,
  onChange,
}: {
  hostname: string;
  state: DashboardState;
  onChange(state: DashboardState): void;
}) {
  const current = useCurrentVisitors(hostname);
  if (current.data === undefined) return null;

  return (
    <button
      type="button"
      className="flex h-8 items-center gap-2 rounded-full bg-card px-3 text-sm surface transition-colors hover:bg-secondary focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
      onClick={() => onChange({ ...state, period: 'realtime', date: null, comparison: 'off' })}
    >
      <span className="size-2 animate-pulse rounded-full bg-success" aria-hidden />
      {current.data} current visitor{current.data === 1 ? '' : 's'}
    </button>
  );
}

/** Shown while a site isn't counting: what it would take, and the switch that does it. */
function CountingOff({ hostname }: { hostname: string }) {
  const update = useUpdateHost();
  const { toast } = useToast();

  return (
    <div className="flex flex-wrap items-center justify-between gap-4 rounded-2xl bg-card p-4 surface">
      <div className="space-y-0.5">
        <p className="text-sm font-medium">This site isn&apos;t counting visits.</p>
        <p className="text-xs text-muted-foreground">No cookies, and nothing that identifies a visitor is kept.</p>
      </div>
      <label className="flex items-center gap-3 text-sm">
        <Switch
          disabled={update.isPending}
          onCheckedChange={async (analytics) => {
            try {
              await update.mutateAsync({ hostname, patch: { analytics } });
              toast({ title: 'Counting visits to this site' });
            } catch (error) {
              toast({
                title: "Couldn't turn analytics on",
                description: error instanceof Error ? error.message : String(error),
                variant: 'destructive',
              });
            }
          }}
        />
        Count visits
      </label>
    </div>
  );
}

/** Today, in the browser's calendar. */
function today(): string {
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

export default AnalyticsPage;
