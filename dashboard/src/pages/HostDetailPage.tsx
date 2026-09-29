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

import { AddressInput } from '@/components/AddressInput';
import { CopyButton } from '@/components/CopyButton';
import { DnsRecords } from '@/components/DnsRecords';
import { Row, Section, ToggleRow } from '@/components/Grouped';
import { FileList } from '@/components/host/FileList';
import { GrantsEditor } from '@/components/host/GrantsEditor';
import { Nip05Editor } from '@/components/host/Nip05Editor';
import { VersionList } from '@/components/host/VersionList';
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
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog';
import { Skeleton } from '@/components/ui/skeleton';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { useSparklines } from '@/hooks/useAnalytics';
import { useDeleteHost, useHost, useMe, useSyncHost, useUpdateHost } from '@/hooks/useNpanel';
import { useParsedAddress } from '@/hooks/useParsedAddress';
import { useToast } from '@/hooks/useToast';
import type { Host, HostPatch } from '@/lib/api-types';
import { compactNumber } from '@/lib/format';
import { cn } from '@/lib/utils';
import { isRootDomain } from '@shared/hostname';

const BACK = { to: '/', label: 'Sites' };

/** Everything about one site: how it's doing, what it serves, how, and who may change it. */
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

      <Tabs defaultValue="general" className="gap-6">
        {/* Scrolls sideways on a narrow phone. Clipped vertically: overflow-x
            alone makes overflow-y auto too, and the triggers' underline sits a
            few pixels below the list, which gave it a scrollbar of nothing. */}
        <div className="-mx-4 overflow-x-auto overflow-y-hidden px-4 [scrollbar-width:none] sm:mx-0 sm:px-0">
          <TabsList>
            <TabsTrigger value="general" className="px-3">
              General
            </TabsTrigger>
            <TabsTrigger value="versions" className="px-3">
              Versions
            </TabsTrigger>
            <TabsTrigger value="files" className="px-3">
              Files
            </TabsTrigger>
            <TabsTrigger value="nip05" className="px-3">
              NIP-05
            </TabsTrigger>
            {me.data?.role === 'admin' && (
              <TabsTrigger value="access" className="px-3">
                Access
              </TabsTrigger>
            )}
          </TabsList>
        </div>

        <TabsContent value="general">
          <General host={data} />
        </TabsContent>
        <TabsContent value="versions">
          <VersionList hostname={data.hostname} />
        </TabsContent>
        <TabsContent value="files">
          <FileList hostname={data.hostname} />
        </TabsContent>
        <TabsContent value="nip05">
          <Nip05Editor hostname={data.hostname} />
        </TabsContent>
        {me.data?.role === 'admin' && (
          <TabsContent value="access">
            <GrantsEditor hostname={data.hostname} />
          </TabsContent>
        )}
      </Tabs>
    </div>
  );
}

/**
 * The site's share image, as a banner the header sits on: the picture it chose
 * for itself, faded out so the page's own text stays readable over it.
 */
function Banner({ src }: { src: string }) {
  const [failed, setFailed] = useState(false);
  if (failed) return null;

  return (
    <img
      src={src}
      alt=""
      onError={() => setFailed(true)}
      className="h-40 w-full rounded-2xl object-cover opacity-80 [mask-image:linear-gradient(to_bottom,black,rgb(0_0_0/0.5)_45%,transparent_90%)] sm:h-52"
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

function General({ host }: { host: Host }) {
  const updateHost = useUpdateHost();
  const { toast } = useToast();

  const patch = async (values: HostPatch, success: string) => {
    try {
      await updateHost.mutateAsync({ hostname: host.hostname, patch: values });
      toast({ title: success });
    } catch (error) {
      toast({
        title: "Couldn't save that",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <div className="space-y-8">
      <Section title="nsite">
        <Row
          label={<PubkeyLabel pubkey={host.pubkey} />}
          description={<span className="block truncate font-mono">{host.address}</span>}
        >
          <div className="flex items-center gap-1">
            <CopyButton value={host.address} label="Copy the nsite address" />
            <RetargetDialog host={host} />
          </div>
        </Row>
        {host.onion && (
          <Row label="Tor" description={<span className="block truncate font-mono">{host.onion}</span>}>
            <CopyButton value={`http://${host.onion}`} label="Copy the onion address" />
          </Row>
        )}
      </Section>

      <Section title="Serving">
        <ToggleRow
          id="enabled"
          label="Serve this site"
          description={host.enabled ? undefined : 'Every request gets a 404.'}
          checked={host.enabled}
          onCheckedChange={(enabled) => patch({ enabled }, enabled ? 'Site turned on' : 'Site turned off')}
        />
        <ToggleRow
          id="spa"
          label="Single-page app"
          description="Unknown paths load the app instead of a 404."
          checked={host.spa}
          onCheckedChange={(spa) => patch({ spa }, spa ? 'Serving as a single-page app' : 'Serving as a static site')}
        />
        <ToggleRow
          id="merge"
          label="Keep files from earlier deploys"
          description="Pages already open keep loading their old chunks."
          checked={host.merge_deployments}
          onCheckedChange={(merge) =>
            patch({ merge_deployments: merge }, merge ? 'Keeping earlier files' : 'Serving the latest deploy only')
          }
        />
        {/* Only a root domain owns its `www`; on a subdomain the switch would be
            a promise npanel has no business making about someone's zone. */}
        {isRootDomain(host.hostname) && (
          <>
            <ToggleRow
              id="www"
              label={`Redirect www.${host.hostname}`}
              checked={host.redirect_www}
              onCheckedChange={(redirect) =>
                patch({ redirect_www: redirect }, redirect ? 'www now redirects here' : 'www no longer redirects here')
              }
            />
            {host.redirect_www && (
              <div className="pr-4 pb-3 pl-4">
                <DnsRecords hostname={host.hostname} www />
              </div>
            )}
          </>
        )}
      </Section>

      <Section
        title="Privacy & Security"
        footer={
          host.csp_policy && (
            <details>
              <summary className="cursor-pointer text-primary select-none">Show the policy</summary>
              <p className="mt-2 font-mono break-all">{host.csp_policy}</p>
            </details>
          )
        }
      >
        <ToggleRow
          id="csp"
          label="Content Security Policy"
          description={cspDescription(host)}
          checked={host.csp}
          onCheckedChange={(csp) => patch({ csp }, csp ? 'Content Security Policy on' : 'Content Security Policy off')}
        />
        <ToggleRow
          id="analytics"
          label="Analytics"
          description="Counts visits without cookies or personal data."
          checked={host.analytics}
          onCheckedChange={(analytics) =>
            patch({ analytics }, analytics ? 'Counting visits' : 'No longer counting visits')
          }
        />
        <ToggleRow
          id="listed"
          label="Show on the public site list"
          checked={!host.hidden}
          onCheckedChange={(listed) => patch({ hidden: !listed }, listed ? 'Site is now listed' : 'Site is now unlisted')}
        />
        <ToggleRow
          id="seo"
          label="Search engine defaults"
          description="A sitemap and canonical URLs, when the site has none."
          checked={host.seo}
          onCheckedChange={(seo) => patch({ seo }, seo ? 'Search engine defaults on' : 'Search engine defaults off')}
        />
      </Section>

      <DeleteSite hostname={host.hostname} />
    </div>
  );
}

function cspDescription(host: Host): string {
  if (host.csp_source === 'off') return 'Off. Any script a deploy includes will run.';
  if (host.csp_source === 'site') return "Using the site's own policy, from its index.html.";
  return 'Scripts may only load from this domain.';
}

/** Point the domain at a different nsite. */
function RetargetDialog({ host }: { host: Host }) {
  const [open, setOpen] = useState(false);
  const [address, setAddress] = useState('');
  const parsed = useParsedAddress(address);
  const updateHost = useUpdateHost();
  const { toast } = useToast();

  const save = async () => {
    try {
      await updateHost.mutateAsync({ hostname: host.hostname, patch: { address } });
      toast({ title: `${host.hostname} now serves the new nsite` });
      setOpen(false);
      setAddress('');
    } catch (error) {
      toast({
        title: "Couldn't change the nsite",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline" size="sm" className="rounded-full">
          Change…
        </Button>
      </DialogTrigger>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Change nsite</DialogTitle>
          <DialogDescription>Serve a different nsite from {host.hostname}.</DialogDescription>
        </DialogHeader>
        <form
          className="space-y-5"
          onSubmit={(event) => {
            event.preventDefault();
            void save();
          }}
        >
          <AddressInput value={address} onChange={setAddress} parsed={parsed} id="retarget" />
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button type="submit" disabled={!parsed.address || updateHost.isPending}>
              Change
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
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
