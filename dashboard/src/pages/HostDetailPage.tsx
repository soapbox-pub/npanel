import { useSeoMeta } from '@unhead/react';
import { ArrowLeft, ExternalLink, RefreshCw } from 'lucide-react';
import { useState } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';

import { AddressInput } from '@/components/AddressInput';
import { DnsRecords } from '@/components/DnsRecords';
import { useParsedAddress } from '@/hooks/useParsedAddress';
import { FileList } from '@/components/host/FileList';
import { GrantsEditor } from '@/components/host/GrantsEditor';
import { VersionList } from '@/components/host/VersionList';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { StatusBadge } from '@/components/StatusBadge';
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
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { useDeleteHost, useHost, useMe, useSyncHost, useUpdateHost } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { HostPatch } from '@/lib/api-types';
import { isRootDomain } from '@shared/hostname';

/** Everything about one site: what it points at, its versions, its access. */
export function HostDetailPage() {
  const params = useParams();
  const hostname = params.hostname;
  const navigate = useNavigate();
  const { toast } = useToast();

  const host = useHost(hostname);
  const me = useMe();
  const updateHost = useUpdateHost();
  const deleteHost = useDeleteHost();
  const syncHost = useSyncHost();

  const [address, setAddress] = useState('');
  const parsed = useParsedAddress(address);

  useSeoMeta({ title: host.data ? `${host.data.hostname} — npanel` : 'npanel' });

  if (host.isLoading) return <Skeleton className="h-64 w-full" />;

  if (host.isError || !host.data) {
    return (
      <div className="space-y-4">
        <BackLink />
        <p className="text-sm text-destructive">
          {host.error instanceof Error ? host.error.message : 'Site not found.'}
        </p>
      </div>
    );
  }

  const data = host.data;

  const run = async (action: () => Promise<unknown>, success: string) => {
    try {
      await action();
      toast({ title: success });
    } catch (error) {
      toast({
        title: 'That did not work',
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  const patch = (values: HostPatch, success: string) =>
    run(() => updateHost.mutateAsync({ hostname: data.hostname, patch: values }), success);

  return (
    <div className="space-y-6">
      <BackLink />

      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="space-y-1">
          <div className="flex items-center gap-2">
            <h1 className="text-2xl font-semibold tracking-tight">{data.hostname}</h1>
            <a
              href={`https://${data.hostname}`}
              target="_blank"
              rel="noreferrer noopener"
              className="text-muted-foreground hover:text-foreground"
              aria-label={`Open ${data.hostname}`}
            >
              <ExternalLink className="h-4 w-4" />
            </a>
          </div>
          <div className="flex items-center gap-2 text-sm text-muted-foreground">
            <PubkeyLabel pubkey={data.pubkey} />
            <span>·</span>
            <span>{data.identifier || 'root site'}</span>
          </div>
        </div>

        <div className="flex items-center gap-3">
          <StatusBadge host={data} />
          <Button
            variant="outline"
            size="sm"
            disabled={syncHost.isPending}
            onClick={() =>
              run(() => syncHost.mutateAsync(data.hostname), 'Checking relays for a new version')
            }
          >
            <RefreshCw className="mr-2 h-4 w-4" />
            Sync now
          </Button>
        </div>
      </div>

      {data.latest?.status === 'failed' && data.latest.error && (
        <p className="rounded-md border border-destructive/40 bg-destructive/10 p-3 text-sm">
          Latest deploy failed: {data.latest.error}
        </p>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Target</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <code className="block break-all rounded-md bg-muted p-3 text-xs">{data.address}</code>

          <AddressInput
            value={address}
            onChange={setAddress}
            parsed={parsed}
            label="Change target"
            id="retarget"
          />

          <Button
            size="sm"
            disabled={!parsed.address || updateHost.isPending}
            onClick={() =>
              run(async () => {
                await updateHost.mutateAsync({ hostname: data.hostname, patch: { address } });
                setAddress('');
              }, 'Target updated')
            }
          >
            Update target
          </Button>
        </CardContent>
      </Card>

      <Tabs defaultValue="versions">
        <TabsList>
          <TabsTrigger value="versions">Versions</TabsTrigger>
          <TabsTrigger value="files">Files</TabsTrigger>
          <TabsTrigger value="settings">Settings</TabsTrigger>
          {me.data?.role === 'admin' && <TabsTrigger value="access">Access</TabsTrigger>}
        </TabsList>

        <TabsContent value="versions" className="pt-4">
          <Card>
            <VersionList hostname={data.hostname} />
          </Card>
        </TabsContent>

        <TabsContent value="files" className="pt-4">
          <Card>
            <FileList hostname={data.hostname} />
          </Card>
        </TabsContent>

        <TabsContent value="settings" className="space-y-6 pt-4">
          {/* Only a root domain owns its `www`; on a subdomain the switch would
              be a promise npanel has no business making about someone's zone. */}
          {isRootDomain(data.hostname) && (
            <Card>
              <CardHeader>
                <CardTitle className="text-base">www</CardTitle>
              </CardHeader>
              <CardContent className="space-y-3">
                <label className="flex items-center gap-3 text-sm">
                  <Switch
                    checked={data.redirect_www}
                    onCheckedChange={(redirect) =>
                      patch(
                        { redirect_www: redirect },
                        redirect ? 'www now redirects here' : 'www no longer redirects here',
                      )
                    }
                  />
                  <span className="font-mono text-xs">
                    www.{data.hostname} → {data.hostname}
                  </span>
                </label>
                <p className="text-sm text-muted-foreground">
                  {data.redirect_www
                    ? 'A permanent redirect that keeps the path. It needs the DNS record below: the redirect is an HTTPS response like any other, so www has to reach this server and get a certificate of its own before a browser will follow it.'
                    : `www.${data.hostname} is not served at all, unless you add it as a site of its own.`}
                </p>
                {data.redirect_www && <DnsRecords hostname={data.hostname} www />}
              </CardContent>
            </Card>
          )}

          <Card>
            <CardHeader>
              <CardTitle className="text-base">Visibility</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              {/* The switch reads as "listed", not as "hidden": a site is listed
                  unless someone says otherwise, and a toggle sitting off while
                  the label says Listed would contradict itself. */}
              <label className="flex items-center gap-3 text-sm">
                <Switch
                  checked={!data.hidden}
                  onCheckedChange={(listed) =>
                    patch({ hidden: !listed }, listed ? 'Site is now listed' : 'Site is now unlisted')
                  }
                />
                {data.hidden ? 'Unlisted' : 'Listed'}
              </label>
              <p className="text-sm text-muted-foreground">
                An unlisted site is left out of the list on the homepage, and out of sight of staff
                who can't manage it. It changes nothing about who can visit the site itself.
              </p>
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="text-base">Routing</CardTitle>
            </CardHeader>
            <CardContent className="space-y-2">
              <label className="flex items-center gap-3 text-sm">
                <Switch
                  checked={data.spa}
                  onCheckedChange={(spa) =>
                    patch(
                      { spa },
                      spa ? 'Serving as a single-page app' : 'Serving as a static site',
                    )
                  }
                />
                Single-page app
              </label>
              <p className="text-sm text-muted-foreground">
                {data.spa
                  ? "A path this site has no file for serves its shell — 404.html, or index.html — with a 200, so the app's own router decides what the page is. Missing images, scripts and stylesheets still return 404."
                  : 'A path this site has no file for returns 404, showing its own 404.html if it published one. Turn this on for a site that routes in the browser, or its deep links will be 404s.'}
              </p>
            </CardContent>
          </Card>

          <Card className="border-destructive/40">
            <CardHeader>
              <CardTitle className="text-base text-destructive">Danger zone</CardTitle>
            </CardHeader>
            <CardContent className="flex flex-wrap items-center justify-between gap-4">
              <label className="flex items-center gap-3 text-sm">
                <Switch
                  checked={data.enabled}
                  onCheckedChange={(enabled) =>
                    patch({ enabled }, enabled ? 'Site enabled' : 'Site disabled')
                  }
                />
                {data.enabled ? 'Serving' : 'Disabled — requests return 404'}
              </label>

              <AlertDialog>
                <AlertDialogTrigger asChild>
                  <Button variant="destructive" size="sm">
                    Delete site
                  </Button>
                </AlertDialogTrigger>
                <AlertDialogContent>
                  <AlertDialogHeader>
                    <AlertDialogTitle>Delete {data.hostname}?</AlertDialogTitle>
                    <AlertDialogDescription>
                      The site stops being served. Downloaded files stay on disk until you prune
                      them.
                    </AlertDialogDescription>
                  </AlertDialogHeader>
                  <AlertDialogFooter>
                    <AlertDialogCancel>Cancel</AlertDialogCancel>
                    <AlertDialogAction
                      onClick={() =>
                        run(async () => {
                          await deleteHost.mutateAsync(data.hostname);
                          navigate('/');
                        }, 'Site deleted')
                      }
                    >
                      Delete
                    </AlertDialogAction>
                  </AlertDialogFooter>
                </AlertDialogContent>
              </AlertDialog>
            </CardContent>
          </Card>
        </TabsContent>

        {me.data?.role === 'admin' && (
          <TabsContent value="access" className="pt-4">
            <Card>
              <CardContent className="pt-6">
                <GrantsEditor hostname={data.hostname} />
              </CardContent>
            </Card>
          </TabsContent>
        )}
      </Tabs>
    </div>
  );
}

function BackLink() {
  return (
    <Link to="/" className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground">
      <ArrowLeft className="h-4 w-4" />
      All sites
    </Link>
  );
}

export default HostDetailPage;
