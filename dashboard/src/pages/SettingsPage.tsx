import { useSeoMeta } from '@unhead/react';
import { useState } from 'react';

import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { Textarea } from '@/components/ui/textarea';
import { useDnsTarget, useSettings, useStatus, useUpdateSettings } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import { decodePubkey, formatBytes } from '@/lib/format';

interface Draft {
  relays?: string;
  servers?: string;
  maxFileSize?: string;
  deployAttempts?: string;
  resyncInterval?: string;
  allowedHostSuffixes?: string;
  dnsTargets?: string;
  tlsCheckLimitPerHost?: string;
  tlsCheckLimitTotal?: string;
  contactNpub?: string;
  publicSiteList?: boolean;
}

/**
 * Runtime configuration, stored in the database rather than the environment.
 *
 * Changes take effect immediately: the relay pool resubscribes and the resync
 * timer re-arms without restarting the server.
 */
export function SettingsPage() {
  useSeoMeta({ title: 'Settings — npanel' });

  const settings = useSettings();
  const status = useStatus();
  const dns = useDnsTarget();
  const updateSettings = useUpdateSettings();
  const { toast } = useToast();

  // Fields fall back to the server's values until they are edited, so no
  // effect is needed to copy loaded settings into state.
  const [draft, setDraft] = useState<Draft>({});

  if (settings.isLoading) return <Skeleton className="h-96 w-full" />;

  const current = settings.data?.settings;
  const relays = draft.relays ?? current?.relays.join('\n') ?? '';
  const servers = draft.servers ?? current?.blossom_fallback_servers.join('\n') ?? '';
  const maxFileSize = draft.maxFileSize ?? String(Math.round((current?.max_file_size ?? 0) / (1024 * 1024)));
  const deployAttempts = draft.deployAttempts ?? String(current?.deploy_attempts ?? '');
  const resyncInterval = draft.resyncInterval ?? String(current?.resync_interval ?? '');
  const allowedHostSuffixes = draft.allowedHostSuffixes ?? current?.allowed_host_suffixes.join('\n') ?? '';
  const dnsTargets = draft.dnsTargets ?? current?.dns_targets.join('\n') ?? '';
  const tlsCheckLimitPerHost = draft.tlsCheckLimitPerHost ?? String(current?.tls_check_limit_per_host ?? '');
  const tlsCheckLimitTotal = draft.tlsCheckLimitTotal ?? String(current?.tls_check_limit_total ?? '');
  const contactNpub = draft.contactNpub ?? current?.contact_npub ?? '';
  const publicSiteList = draft.publicSiteList ?? current?.public_site_list ?? false;

  const save = async () => {
    try {
      const result = await updateSettings.mutateAsync({
        relays: lines(relays),
        blossom_fallback_servers: lines(servers),
        max_file_size: Math.round(Number(maxFileSize) * 1024 * 1024),
        deploy_attempts: Number(deployAttempts),
        resync_interval: Number(resyncInterval),
        allowed_host_suffixes: lines(allowedHostSuffixes),
        dns_targets: lines(dnsTargets),
        tls_check_limit_per_host: Number(tlsCheckLimitPerHost),
        tls_check_limit_total: Number(tlsCheckLimitTotal),
        contact_npub: contactNpub.trim(),
        public_site_list: publicSiteList,
      });

      setDraft({});
      toast({
        title: result.changed.length ? 'Settings saved' : 'No changes',
        description: result.changed.length ? `Updated: ${result.changed.join(', ')}` : undefined,
      });
    } catch (error) {
      toast({
        title: "Couldn't save settings",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-2xl font-semibold tracking-tight">Settings</h1>
        <p className="text-sm text-muted-foreground">
          Applied immediately. Every change is recorded in the audit log.
        </p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Public contact</CardTitle>
          <CardDescription>
            Whoever runs this instance, shown on the homepage to signed-out visitors so they can
            tell whose gateway they landed on. An npub, an nprofile, or a hex pubkey — all are
            stored as an npub. Leave it empty to name nobody.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-2">
          <Input
            id="contact-npub"
            value={contactNpub}
            onChange={(event) => setDraft({ ...draft, contactNpub: event.target.value })}
            placeholder="npub1…"
            autoComplete="off"
            spellCheck={false}
            className="font-mono text-sm"
          />
          <ContactPreview value={contactNpub} />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Public site list</CardTitle>
          <CardDescription>
            List the sites this gateway serves on the homepage, where anyone can read them, and
            show staff the sites they don't manage. Off by default — on a private gateway, which
            sites it serves is often the part worth keeping to yourself. Any site can be left out
            individually by marking it unlisted.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <label className="flex items-center gap-3 text-sm">
            <Switch
              checked={publicSiteList}
              onCheckedChange={(checked) => setDraft({ ...draft, publicSiteList: checked })}
            />
            {publicSiteList ? 'Listed publicly' : 'Not listed'}
          </label>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Relays</CardTitle>
          <CardDescription>
            One per line. Used to watch for new site manifests and to look up authors' relay lists
            (kind 10002) and Blossom servers (kind 10063).
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Textarea
            value={relays}
            onChange={(event) => setDraft({ ...draft, relays: event.target.value })}
            rows={6}
            spellCheck={false}
            className="font-mono text-sm"
          />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Blossom fallback servers</CardTitle>
          <CardDescription>
            Only used when a manifest names no servers and its author publishes no kind 10063 list.
            Most sites need none. One per line.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Textarea
            value={servers}
            onChange={(event) => setDraft({ ...draft, servers: event.target.value })}
            rows={4}
            spellCheck={false}
            className="font-mono text-sm"
            placeholder="https://blossom.example.com/"
          />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Limits</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="max-file-size">Maximum file size (MB)</Label>
            <Input
              id="max-file-size"
              type="number"
              min={1}
              value={maxFileSize}
              onChange={(event) => setDraft({ ...draft, maxFileSize: event.target.value })}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="resync-interval">Resync interval (seconds)</Label>
            <Input
              id="resync-interval"
              type="number"
              min={60}
              value={resyncInterval}
              onChange={(event) => setDraft({ ...draft, resyncInterval: event.target.value })}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="deploy-attempts">Deploy attempts</Label>
            <Input
              id="deploy-attempts"
              type="number"
              min={1}
              max={10}
              value={deployAttempts}
              onChange={(event) => setDraft({ ...draft, deployAttempts: event.target.value })}
            />
            <p className="text-sm text-muted-foreground">
              How many times a failed deploy is tried before npanel waits for the next resync. Each
              attempt waits twice as long as the last, and keeps the files already downloaded. Set
              it to 1 to never retry.
            </p>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">DNS target</CardTitle>
          <CardDescription>
            What staff are told to point a domain at when they add a site. One per line: an IP address
            becomes an A or AAAA record, a domain becomes a CNAME and must be the only entry. Leave
            this empty to use whatever {dns.data?.dashboard_host ?? 'the dashboard domain'} itself
            resolves to.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-2">
          <Textarea
            value={dnsTargets}
            onChange={(event) => setDraft({ ...draft, dnsTargets: event.target.value })}
            rows={3}
            spellCheck={false}
            className="font-mono text-sm"
            placeholder="203.0.113.10"
          />
          {!dnsTargets.trim() && dns.data && (
            <p className="text-sm text-muted-foreground">
              {dns.data.records.length
                ? `Detected: ${dns.data.records.map((record) => `${record.type} ${record.value}`).join(', ')}`
                : `Nothing detected — ${dns.data.dashboard_host} doesn't resolve from this server, so staff are shown no records.`}
            </p>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Allowed domains</CardTitle>
          <CardDescription>
            One per line, and a site's domain must match one of them or sit under it. Adding a site
            authorizes a certificate for it, and certificate authorities meter issuance per
            registered domain — so this limits how much of that budget any one person can spend.
            Leave empty to allow any domain. Sites that already exist keep working.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <Textarea
            value={allowedHostSuffixes}
            onChange={(event) => setDraft({ ...draft, allowedHostSuffixes: event.target.value })}
            rows={4}
            spellCheck={false}
            className="font-mono text-sm"
            placeholder="example.com"
          />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Certificate authorization</CardTitle>
          <CardDescription>
            How many certificates npanel will approve per hour when your proxy asks, per domain and
            in total. A site whose DNS doesn't point here would otherwise retry forever and
            exhaust your certificate authority's issuance limits. The dashboard's own domain is
            always approved.
          </CardDescription>
        </CardHeader>
        <CardContent className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="tls-check-limit-per-host">Per domain, each hour</Label>
            <Input
              id="tls-check-limit-per-host"
              type="number"
              min={1}
              value={tlsCheckLimitPerHost}
              onChange={(event) => setDraft({ ...draft, tlsCheckLimitPerHost: event.target.value })}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="tls-check-limit-total">Across all domains, each hour</Label>
            <Input
              id="tls-check-limit-total"
              type="number"
              min={1}
              value={tlsCheckLimitTotal}
              onChange={(event) => setDraft({ ...draft, tlsCheckLimitTotal: event.target.value })}
            />
          </div>
        </CardContent>
      </Card>

      <div className="flex items-center gap-4">
        <Button onClick={save} disabled={updateSettings.isPending}>
          {updateSettings.isPending ? 'Saving…' : 'Save settings'}
        </Button>

        {status.data && (
          <p className="text-sm text-muted-foreground">
            {status.data.hosts} sites · {status.data.blobs} blobs · {formatBytes(status.data.blob_bytes)}{' '}
            stored
          </p>
        )}
      </div>
    </div>
  );
}

/** Who the pasted identifier turns out to be, before it's saved. */
function ContactPreview({ value }: { value: string }) {
  const pubkey = decodePubkey(value);

  if (!value.trim()) {
    return <p className="text-sm text-muted-foreground">Nobody is named on the homepage.</p>;
  }

  if (!pubkey) {
    return <p className="text-sm text-destructive">That isn't an npub.</p>;
  }

  return (
    <p className="flex items-center gap-2 text-sm text-muted-foreground">
      Shown as <PubkeyLabel pubkey={pubkey} className="text-foreground" />
    </p>
  );
}

function lines(value: string): string[] {
  return value
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
}

export default SettingsPage;
