import { useSeoMeta } from '@unhead/react';
import {
  ArrowDownRight,
  ArrowUpRight,
  BarChart3,
  ChevronRight,
  RefreshCw,
} from 'lucide-react';
import { useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';

import { LinkRow, RowIcon, Section } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { SiteIcon } from '@/components/SiteIcon';
import { SiteStatus } from '@/components/SiteStatus';
import { Sparkline } from '@/components/Sparkline';
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/components/ui/alert-dialog';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';
import { useSparklines } from '@/hooks/useAnalytics';
import { useDeleteHost, useHost, useMe, useSyncHost } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { Host } from '@/lib/api-types';
import { compactNumber, timeAgo } from '@/lib/format';
import { SITE_SECTIONS, type SiteSectionId } from '@/lib/site-sections';
import { cn } from '@/lib/utils';

const BACK = { to: '/', label: 'Sites' };

/**
 * One site: how it presents itself, how it's doing, and a page for each part
 * of its settings, the way Settings opens onto its panes.
 */
export function HostDetailPage() {
  const hostname = useParams().hostname;
  const host = useHost(hostname);
  const me = useMe();
  const syncHost = useSyncHost();
  const { toast } = useToast();

  useSeoMeta({ title: host.data ? `${host.data.hostname} — npanel` : 'npanel' });

  if (host.isLoading) {
    return (
      <div className="space-y-6">
        <Skeleton className="h-16 w-72 rounded-xl" />
        <Skeleton className="h-40 rounded-2xl" />
      </div>
    );
  }

  if (host.isError || !host.data) {
    return (
      <div className="space-y-6">
        <PageHeader back={BACK} title="Site not found" />
        <p className="text-muted-foreground">
          {host.error instanceof Error ? host.error.message : 'There is no site by that name.'}
        </p>
      </div>
    );
  }

  const data = host.data;

  const sync = async () => {
    try {
      await syncHost.mutateAsync(data.hostname);
      toast({ title: 'Checking for a new version' });
    } catch (error) {
      toast({
        title: "Couldn't check for a new version",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  const { preview } = data;

  return (
    <div className="space-y-8">
      <PageHeader
        back={BACK}
        banner={preview.image && <Banner src={preview.image} />}
        icon={<SiteIcon hostname={data.hostname} src={preview.icon} className="size-14" />}
        title={<span title={preview.title ?? undefined}>{preview.title ?? data.hostname}</span>}
        subtitle={
          <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
            {preview.title && <span className="w-full truncate text-foreground/80">{data.hostname}</span>}
            <SiteStatus host={data} />
            <span aria-hidden>·</span>
            <PubkeyLabel pubkey={data.pubkey} compact />
            {data.identifier && (
              <>
                <span aria-hidden>·</span>
                <span className="truncate">{data.identifier}</span>
              </>
            )}
          </span>
        }
        actions={
          <>
            <Button
              variant="outline"
              size="icon"
              className="rounded-full"
              disabled={syncHost.isPending}
              onClick={sync}
              aria-label="Check for a new version"
              title="Check for a new version"
            >
              <RefreshCw className={cn(syncHost.isPending && 'animate-spin')} aria-hidden />
            </Button>
            <Button asChild className="rounded-full">
              <a href={`https://${data.hostname}`} target="_blank" rel="noreferrer noopener">
                Visit
                <ArrowUpRight aria-hidden />
              </a>
            </Button>
          </>
        }
      />

      {data.latest?.status === 'failed' && data.latest.error && (
        <p className="rounded-xl bg-destructive/10 p-4 text-sm [overflow-wrap:anywhere]">
          <span className="font-semibold text-destructive">The latest deploy failed.</span> {data.latest.error}
        </p>
      )}

      <Visitors host={data} />

      <SiteMenu host={data} admin={me.data?.role === 'admin'} />

      <DeleteSite hostname={data.hostname} />
    </div>
  );
}

/**
 * The site's share image, as a banner the header sits on: the picture it chose
 * for itself, faded out so the page's own text stays readable over it. Left
 * off a phone, where it would push everything that matters below the fold.
 */
function Banner({ src }: { src: string }) {
  const [failed, setFailed] = useState(false);
  if (failed) return null;

  return (
    <img
      src={src}
      alt=""
      onError={() => setFailed(true)}
      className="hidden h-52 w-full rounded-2xl object-cover opacity-80 sm:block [mask-image:linear-gradient(to_bottom,black,rgb(0_0_0/0.5)_45%,transparent_90%)]"
    />
  );
}

/** The site's last day at a glance, and the way into its analytics. */
function Visitors({ host }: { host: Host }) {
  const sparklines = useSparklines();
  const sparkline = sparklines.data?.[host.hostname];
  const change = sparkline?.change ?? null;

  return (
    <Link
      to={`/hosts/${host.hostname}/analytics`}
      className="group flex items-center gap-6 rounded-2xl bg-card p-5 surface transition-shadow hover:surface-lift focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden "
    >
      <div className="shrink-0 space-y-1">
        <p className="flex items-center gap-1.5 text-sm font-medium text-primary">
          <BarChart3 className="size-4" aria-hidden />
          Analytics
        </p>
        {!host.analytics && !sparkline?.visitors ? (
          <p className="text-sm text-muted-foreground">Not counting visits</p>
        ) : sparklines.isLoading ? (
          <Skeleton className="h-8 w-24" />
        ) : (
          <>
            <p className="flex items-baseline gap-2">
              <span className="text-2xl font-bold tabular-nums">{compactNumber(sparkline?.visitors ?? 0)}</span>
              {change !== null && change !== 0 && (
                <span
                  className={cn(
                    'flex items-center text-sm font-medium tabular-nums',
                    change > 0 ? 'text-success' : 'text-destructive',
                  )}
                >
                  {change > 0 ? <ArrowUpRight className="size-3.5" /> : <ArrowDownRight className="size-3.5" />}
                  {Math.abs(change)}%
                </span>
              )}
            </p>
            <p className="text-xs text-muted-foreground">visitors in last 24h</p>
          </>
        )}
      </div>

      <Sparkline
        values={sparkline?.hours ?? []}
        className={cn('h-14 min-w-0 flex-1', host.analytics ? 'text-primary' : 'text-muted-foreground/40')}
      />
      <ChevronRight className="size-5 shrink-0 text-muted-foreground/60" aria-hidden />
    </Link>
  );
}

/** Every page of the site's settings, as Settings lists its panes. */
function SiteMenu({ host, admin }: { host: Host; admin: boolean }) {
  return (
    <div className="space-y-6">
      {SITE_SECTIONS.map((group) => (
        <Section key={group[0].id}>
          {group
            .filter((section) => admin || !section.adminOnly)
            .map((section) => (
              <LinkRow
                key={section.id}
                to={`/hosts/${host.hostname}/${section.id}`}
                icon={
                  <RowIcon color={section.color}>
                    <section.icon />
                  </RowIcon>
                }
                label={section.label}
                value={summary(section.id, host)}
              />
            ))}
        </Section>
      ))}
    </div>
  );
}

/** A word or two of what a section holds, beside its name, where there's one worth saying. */
function summary(id: SiteSectionId, host: Host): string | undefined {
  switch (id) {
    case 'nsite':
      return host.identifier || 'Root site';
    case 'serving':
      if (!host.enabled) return 'Off';
      return host.spa ? 'Single-page app' : 'Static site';
    case 'privacy':
      return host.analytics ? 'Analytics on' : undefined;
    case 'seo':
      return host.hidden ? 'Unlisted' : undefined;
    case 'versions':
      if (host.pinned_event_id) return 'Pinned';
      return host.active ? `Updated ${timeAgo(host.active.created_at)}` : undefined;
    case 'files':
      return host.active ? compactNumber(host.active.files_total) : undefined;
    default:
      return undefined;
  }
}

function DeleteSite({ hostname }: { hostname: string }) {
  const deleteHost = useDeleteHost();
  const navigate = useNavigate();
  const { toast } = useToast();

  const remove = async () => {
    try {
      await deleteHost.mutateAsync(hostname);
      toast({ title: `${hostname} deleted` });
      navigate('/');
    } catch (error) {
      toast({
        title: "Couldn't delete the site",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <AlertDialog>
      <AlertDialogTrigger asChild>
        <button
          type="button"
          className="flex h-12 w-full items-center justify-center gap-2 rounded-xl bg-card text-sm text-destructive surface transition-colors hover:bg-destructive/5 focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden "
        >
          Delete Site…
        </button>
      </AlertDialogTrigger>
      <AlertDialogContent size="sm">
        <AlertDialogHeader>
          <AlertDialogTitle>Delete {hostname}?</AlertDialogTitle>
          <AlertDialogDescription>
            It stops being served right away. Its files stay on disk until they're pruned.
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction variant="destructive" onClick={remove}>
            Delete
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}

export default HostDetailPage;
