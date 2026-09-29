import { useSeoMeta } from '@unhead/react';
import { ArrowDownRight, ArrowUpRight, Globe, Search } from 'lucide-react';
import { type ReactNode, useState } from 'react';
import { Link } from 'react-router-dom';

import { AddHostDialog } from '@/components/AddHostDialog';
import { PageHeader } from '@/components/PageHeader';
import { SiteIcon } from '@/components/SiteIcon';
import { SiteStatus } from '@/components/SiteStatus';
import { Sparkline } from '@/components/Sparkline';
import { Input } from '@/components/ui/input';
import { Skeleton } from '@/components/ui/skeleton';
import { useSparklines } from '@/hooks/useAnalytics';
import { useHosts } from '@/hooks/useNpanel';
import type { Host, Sparkline as SparklineData } from '@/lib/api-types';
import { compactNumber } from '@/lib/format';
import { siteStatus } from '@/lib/site-status';
import { cn } from '@/lib/utils';

/**
 * Every site this user manages, laid out as Plausible lays out its list of
 * sites: a card each, with the last 24 hours drawn on it, so which sites are
 * busy — and which have gone quiet — shows before anything is opened.
 */
export function HostsPage() {
  useSeoMeta({ title: 'Sites — npanel' });

  const hosts = useHosts();
  const sparklines = useSparklines();
  const [search, setSearch] = useState('');

  const mine = hosts.data?.hosts ?? [];
  const others = hosts.data?.other_sites ?? [];
  const needle = search.trim().toLowerCase();
  const shown = needle ? mine.filter((host) => host.hostname.includes(needle)) : mine;

  return (
    <div className="space-y-6">
      <PageHeader title="Sites" actions={<AddHostDialog />} />

      {mine.length > 6 && (
        <div className="relative max-w-xs">
          <Search className="absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" aria-hidden />
          <Input
            type="search"
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Search"
            aria-label="Search sites"
            className="surface rounded-full border-transparent pl-9"
          />
        </div>
      )}

      {hosts.isLoading ? (
        <Grid>
          {[0, 1, 2].map((index) => (
            <Skeleton key={index} className="h-52 rounded-2xl" />
          ))}
        </Grid>
      ) : hosts.isError ? (
        <p className="rounded-xl bg-destructive/10 p-4 text-sm text-destructive">
          {hosts.error instanceof Error ? hosts.error.message : "Couldn't load your sites. Reload the page to try again."}
        </p>
      ) : !mine.length ? (
        <Empty />
      ) : !shown.length ? (
        <p className="py-16 text-center text-muted-foreground">No sites match “{search.trim()}”.</p>
      ) : (
        <Grid>
          {shown.map((host) => (
            <li key={host.hostname}>
              <SiteCard
                host={host}
                sparkline={sparklines.data?.[host.hostname]}
                loading={sparklines.isLoading}
              />
            </li>
          ))}
        </Grid>
      )}

      {others.length > 0 && <OtherSites hostnames={others.map((site) => site.hostname)} />}
    </div>
  );
}

function Grid({ children }: { children: ReactNode }) {
  return <ul className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">{children}</ul>;
}

function SiteCard({ host, sparkline, loading }: { host: Host; sparkline?: SparklineData; loading: boolean }) {
  const status = siteStatus(host);

  const { preview } = host;

  return (
    <Link
      to={`/hosts/${host.hostname}`}
      className="group flex h-full flex-col overflow-hidden rounded-2xl bg-card surface transition-[box-shadow,transform] duration-200 hover:surface-lift focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden active:scale-[0.99] dark:hover:bg-secondary/40"
    >
      <div className="flex flex-1 flex-col gap-4 p-5">
        <div className="flex items-center gap-3">
          <SiteIcon hostname={host.hostname} src={preview.icon} className="size-9" />
          <div className="min-w-0 flex-1">
            <p className="truncate font-semibold">{preview.title ?? host.hostname}</p>
            {preview.title && <p className="truncate text-xs text-muted-foreground">{host.hostname}</p>}
          </div>
        </div>

        {preview.description && (
          <p className="line-clamp-2 text-sm text-muted-foreground">{preview.description}</p>
        )}

        {host.analytics &&
          (loading ? (
            <div className="mt-auto space-y-3" aria-hidden>
              <Skeleton className="h-10" />
              <Skeleton className="h-10 w-1/2" />
            </div>
          ) : (
            <div className="mt-auto space-y-3">
              <Sparkline values={sparkline?.hours ?? []} className="h-10 text-primary" />
              <div className="flex items-end justify-between gap-3">
                <div>
                  <p className="text-xl font-bold tabular-nums">{compactNumber(sparkline?.visitors ?? 0)}</p>
                  <p className="text-sm text-muted-foreground">
                    {sparkline?.visitors === 1 ? 'visitor' : 'visitors'} in last 24h
                  </p>
                </div>
                {status.tone === 'green' ? (
                  <Change value={sparkline?.change ?? null} />
                ) : (
                  <SiteStatus host={host} className="text-xs" />
                )}
              </div>
            </div>
          ))}

        {/* Without analytics there are no numbers to sit beside, so a site that
            needs attention says so on its own line. */}
        {!host.analytics && status.tone !== 'green' && <SiteStatus host={host} className="mt-auto text-xs" />}
      </div>
    </Link>
  );
}

/** Visitors up or down on the day before, as Plausible marks it. */
function Change({ value }: { value: number | null }) {
  // No change is no news, and a "0%" beside the count only adds noise.
  if (!value) return null;
  const Arrow = value > 0 ? ArrowUpRight : ArrowDownRight;

  return (
    <p className="flex items-center gap-0.5 text-sm font-medium tabular-nums">
      <Arrow className={cn('size-3.5', value > 0 ? 'text-success' : 'text-destructive')} aria-hidden />
      <span className="sr-only">{value > 0 ? 'Up' : 'Down'}</span>
      {Math.abs(value)}%
    </p>
  );
}

function Empty() {
  return (
    <div className="flex flex-col items-center gap-4 rounded-2xl border-2 border-dashed px-8 py-16 text-center">
      <span className="flex size-14 items-center justify-center rounded-2xl bg-primary/10 text-primary">
        <Globe className="size-7" aria-hidden />
      </span>
      <div className="space-y-1">
        <p className="text-lg font-semibold">No sites yet</p>
        <p className="max-w-xs text-sm text-muted-foreground">Add a domain and the nsite it should serve.</p>
      </div>
      <AddHostDialog />
    </div>
  );
}

/**
 * What else this gateway serves: names only, so staff can see a domain is
 * taken without asking. Managing one takes a grant from an admin.
 */
function OtherSites({ hostnames }: { hostnames: string[] }) {
  return (
    <section className="space-y-3 pt-6">
      <h2 className="px-1 text-[13px] font-semibold text-foreground/80">Also on this gateway</h2>
      <ul className="flex flex-wrap gap-2">
        {hostnames.map((hostname) => (
          <li key={hostname}>
            <a
              href={`https://${hostname}`}
              target="_blank"
              rel="noreferrer noopener"
              className="inline-flex items-center gap-2 rounded-full bg-card py-1 pr-3 pl-1 text-sm surface hover:text-primary"
            >
              <SiteIcon hostname={hostname} className="size-5" />
              {hostname}
            </a>
          </li>
        ))}
      </ul>
    </section>
  );
}

export default HostsPage;
