import { useSeoMeta } from '@unhead/react';
import { ArrowLeft } from 'lucide-react';
import { Link, useParams, useSearchParams } from 'react-router-dom';

import { DetailsDialog } from '@/components/analytics/DetailsDialog';
import { Filters } from '@/components/analytics/Filters';
import { ComparisonPicker, PeriodPicker } from '@/components/analytics/PeriodPicker';
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

  if (host.isLoading) return <Skeleton className="h-96 w-full" />;
  if (host.isError || !host.data) {
    return (
      <div className="space-y-4">
        <BackLink hostname={hostname} />
        <p className="text-sm text-destructive">
          {host.error instanceof Error ? host.error.message : 'Site not found.'}
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
      <BackLink hostname={hostname} />

      <div className="flex flex-wrap items-center justify-between gap-4">
        <h1 className="text-2xl font-semibold tracking-tight">{hostname}</h1>
        <CurrentVisitors hostname={hostname} state={state} onChange={setState} />
      </div>

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
      className="flex items-center gap-2 text-sm text-muted-foreground hover:text-foreground"
      onClick={() => onChange({ ...state, period: 'realtime', date: null, comparison: 'off' })}
    >
      <span className="size-2 animate-pulse rounded-full bg-green-500" />
      {current.data} current visitor{current.data === 1 ? '' : 's'}
    </button>
  );
}

/** Shown while a site isn't counting: what it would take, and the switch that does it. */
function CountingOff({ hostname }: { hostname: string }) {
  const update = useUpdateHost();
  const { toast } = useToast();

  return (
    <div className="flex flex-wrap items-center justify-between gap-4 rounded-md border border-dashed p-4">
      <p className="max-w-2xl text-sm text-muted-foreground">
        This site isn&apos;t counting visits. Turning it on adds a small script to every page, served from
        the site itself — no cookies, and nothing that identifies a visitor is stored. Anything counted
        before it was turned off is still shown below.
      </p>
      <label className="flex items-center gap-2 text-sm">
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

function BackLink({ hostname }: { hostname: string }) {
  return (
    <Link
      to={`/hosts/${hostname}`}
      className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
    >
      <ArrowLeft className="h-4 w-4" />
      {hostname}
    </Link>
  );
}

/** Today, in the browser's calendar. */
function today(): string {
  const now = new Date();
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
}

export default AnalyticsPage;
