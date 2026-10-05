import { useSeoMeta } from '@unhead/react';
import { type FormEvent, useState } from 'react';
import { useParams } from 'react-router-dom';

import { CertificateRow } from '@/components/CertificateRow';
import { FieldRow, Row, RowIcon, Section, ToggleRow } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { PubkeyLabel } from '@/components/PubkeyLabel';
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
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Textarea } from '@/components/ui/textarea';
import {
  useClearLinkPreviews,
  useDnsTarget,
  useLinkPreviews,
  useRefreshLinkPreview,
  useSettings,
  useStatus,
  useUpdateSettings,
} from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type {
  LinkPreviewStatus,
  RefreshedLinkPreview,
  TlsStatus as TlsStatusView,
  TorStatus as TorStatusView,
} from '@/lib/api-types';
import { decodePubkey, formatBytes } from '@/lib/format';
import { findSettingsSection } from '@/lib/settings-sections';
import { cn } from '@/lib/utils';
import NotFound from '@/pages/NotFound';

const LETS_ENCRYPT = 'https://acme-v02.api.letsencrypt.org/directory';
const LETS_ENCRYPT_STAGING = 'https://acme-staging-v02.api.letsencrypt.org/directory';

interface Draft {
  relays?: string;
  servers?: string;
  maxFileSize?: string;
  maxSiteSize?: string;
  maxSiteFiles?: string;
  deployAttempts?: string;
  resyncInterval?: string;
  keepVersions?: string;
  allowedHostSuffixes?: string;
  dnsTargets?: string;
  tlsCheckLimitPerHost?: string;
  tlsCheckLimitTotal?: string;
  acmeDirectory?: string;
  acmeEmail?: string;
  contactNpub?: string;
  publicSiteList?: boolean;
  torEnabled?: boolean;
  analyticsTimezone?: string;
  crawlerPattern?: string;
  linkPreviewTtl?: string;
  linkPreviewConcurrency?: string;
  linkPreviewMissesPerMinute?: string;
  linkPreviewMaxEntries?: string;
}

/**
 * One page of the gateway's settings, opened from the Settings list.
 *
 * Changes take effect as soon as they're saved: the relay pool resubscribes
 * and the resync timer re-arms without restarting the server.
 */
export function SettingsSectionPage() {
  const section = findSettingsSection(useParams().section);
  useSeoMeta({ title: section ? `${section.label} — Settings — npanel` : 'npanel' });

  const settings = useSettings();
  const status = useStatus();
  const dns = useDnsTarget();
  const updateSettings = useUpdateSettings();
  const { toast } = useToast();

  // Fields fall back to the server's values until they are edited, so no
  // effect is needed to copy loaded settings into state.
  const [draft, setDraft] = useState<Draft>({});

  if (!section) return <NotFound />;
  if (settings.isLoading) return <Skeleton className="h-96 w-full rounded-xl" />;

  const current = settings.data?.settings;
  const relays = draft.relays ?? current?.relays.join('\n') ?? '';
  const servers = draft.servers ?? current?.blossom_fallback_servers.join('\n') ?? '';
  const maxFileSize = draft.maxFileSize ?? String(Math.round((current?.max_file_size ?? 0) / (1024 * 1024)));
  const maxSiteSize = draft.maxSiteSize ?? String(Math.round((current?.max_site_size ?? 0) / (1024 * 1024)));
  const maxSiteFiles = draft.maxSiteFiles ?? String(current?.max_site_files ?? '');
  const deployAttempts = draft.deployAttempts ?? String(current?.deploy_attempts ?? '');
  const resyncInterval = draft.resyncInterval ?? String(current?.resync_interval ?? '');
  const keepVersions = draft.keepVersions ?? String(current?.keep_versions ?? '');
  const allowedHostSuffixes = draft.allowedHostSuffixes ?? current?.allowed_host_suffixes.join('\n') ?? '';
  const dnsTargets = draft.dnsTargets ?? current?.dns_targets.join('\n') ?? '';
  const tlsCheckLimitPerHost = draft.tlsCheckLimitPerHost ?? String(current?.tls_check_limit_per_host ?? '');
  const tlsCheckLimitTotal = draft.tlsCheckLimitTotal ?? String(current?.tls_check_limit_total ?? '');
  const acmeDirectory = draft.acmeDirectory ?? current?.acme_directory ?? LETS_ENCRYPT;
  const acmeEmail = draft.acmeEmail ?? current?.acme_email ?? '';
  const contactNpub = draft.contactNpub ?? current?.contact_npub ?? '';
  const publicSiteList = draft.publicSiteList ?? current?.public_site_list ?? false;
  const torEnabled = draft.torEnabled ?? current?.tor_enabled ?? false;
  const analyticsTimezone = draft.analyticsTimezone ?? current?.analytics_timezone ?? 'UTC';
  const crawlerPattern = draft.crawlerPattern ?? current?.link_preview_crawler_pattern ?? '';
  const linkPreviewTtl = draft.linkPreviewTtl ?? String(current?.link_preview_ttl ?? '');
  const linkPreviewConcurrency = draft.linkPreviewConcurrency ?? String(current?.link_preview_concurrency ?? '');
  const linkPreviewMissesPerMinute =
    draft.linkPreviewMissesPerMinute ?? String(current?.link_preview_misses_per_minute ?? '');
  const linkPreviewMaxEntries = draft.linkPreviewMaxEntries ?? String(current?.link_preview_max_entries ?? '');

  const save = async () => {
    try {
      const result = await updateSettings.mutateAsync({
        relays: lines(relays),
        blossom_fallback_servers: lines(servers),
        max_file_size: Math.round(Number(maxFileSize) * 1024 * 1024),
        max_site_size: Math.round(Number(maxSiteSize) * 1024 * 1024),
        max_site_files: Number(maxSiteFiles),
        deploy_attempts: Number(deployAttempts),
        resync_interval: Number(resyncInterval),
        keep_versions: Number(keepVersions),
        allowed_host_suffixes: lines(allowedHostSuffixes),
        dns_targets: lines(dnsTargets),
        tls_check_limit_per_host: Number(tlsCheckLimitPerHost),
        tls_check_limit_total: Number(tlsCheckLimitTotal),
        acme_directory: acmeDirectory.trim(),
        acme_email: acmeEmail.trim(),
        contact_npub: contactNpub.trim(),
        public_site_list: publicSiteList,
        tor_enabled: torEnabled,
        analytics_timezone: analyticsTimezone,
        link_preview_crawler_pattern: crawlerPattern.trim(),
        link_preview_ttl: Number(linkPreviewTtl),
        link_preview_concurrency: Number(linkPreviewConcurrency),
        link_preview_misses_per_minute: Number(linkPreviewMissesPerMinute),
        link_preview_max_entries: Number(linkPreviewMaxEntries),
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

  const dirty = Object.keys(draft).length > 0;

  return (
    <div className={cn('space-y-8', dirty && 'pb-20')}>
      <PageHeader
        back={{ to: '/settings', label: 'Settings' }}
        icon={
          <RowIcon color={section.color} className="size-10 rounded-[10px] [&_svg]:size-6">
            <section.icon />
          </RowIcon>
        }
        title={section.label}
      />

      {section.id === 'homepage' && (
        <Section footer="Shown to visitors who aren't signed in.">
          <FieldRow label="Run by" htmlFor="contact-npub">
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
          </FieldRow>
          <ToggleRow
            id="public-site-list"
            label="List the sites served here"
            description="Staff also see sites they don't manage. Any site can opt out."
            checked={publicSiteList}
            onCheckedChange={(checked) => setDraft({ ...draft, publicSiteList: checked })}
          />
        </Section>
      )}

      {section.id === 'domains' && (
        <Section footer="Leave the DNS target empty to use whatever the dashboard's own domain resolves to.">
          <FieldRow label="DNS target" htmlFor="dns-targets">
            <Textarea
              id="dns-targets"
              value={dnsTargets}
              onChange={(event) => setDraft({ ...draft, dnsTargets: event.target.value })}
              rows={2}
              spellCheck={false}
              className="font-mono text-sm"
              placeholder={dns.data?.records.map((record) => record.value).join('\n') || '203.0.113.10'}
            />
            <p className="text-xs text-muted-foreground">IP addresses, or one domain for a CNAME. One per line.</p>
          </FieldRow>
          <FieldRow label="Allowed domains" htmlFor="allowed-host-suffixes">
            <Textarea
              id="allowed-host-suffixes"
              value={allowedHostSuffixes}
              onChange={(event) => setDraft({ ...draft, allowedHostSuffixes: event.target.value })}
              rows={3}
              spellCheck={false}
              className="font-mono text-sm"
              placeholder="Any domain"
            />
            <p className="text-xs text-muted-foreground">
              Sites must be on one of these or a subdomain of one. One per line.
            </p>
          </FieldRow>
        </Section>
      )}

      {section.id === 'nostr' && (
        <Section>
          <FieldRow label="Relays" htmlFor="relays">
            <Textarea
              id="relays"
              value={relays}
              onChange={(event) => setDraft({ ...draft, relays: event.target.value })}
              rows={4}
              spellCheck={false}
              className="font-mono text-sm"
            />
          </FieldRow>
          <FieldRow label="Fallback Blossom servers" htmlFor="servers">
            <Textarea
              id="servers"
              value={servers}
              onChange={(event) => setDraft({ ...draft, servers: event.target.value })}
              rows={2}
              spellCheck={false}
              className="font-mono text-sm"
              placeholder="https://blossom.example.com/"
            />
            <p className="text-xs text-muted-foreground">
              For authors who name no Blossom servers of their own. Most sites need none.
            </p>
          </FieldRow>
        </Section>
      )}

      {section.id === 'deploys' && (
        <Section>
          <NumberRow
            id="max-file-size"
            label="Largest file"
            unit="MB"
            min={1}
            value={maxFileSize}
            onChange={(value) => setDraft({ ...draft, maxFileSize: value })}
          />
          <NumberRow
            id="max-site-size"
            label="Largest site"
            description="All of one version's files together."
            unit="MB"
            min={1}
            value={maxSiteSize}
            onChange={(value) => setDraft({ ...draft, maxSiteSize: value })}
          />
          <NumberRow
            id="max-site-files"
            label="Most files in a site"
            unit="files"
            min={1}
            value={maxSiteFiles}
            onChange={(value) => setDraft({ ...draft, maxSiteFiles: value })}
          />
          <NumberRow
            id="resync-interval"
            label="Check relays every"
            unit="seconds"
            min={60}
            value={resyncInterval}
            onChange={(value) => setDraft({ ...draft, resyncInterval: value })}
          />
          <NumberRow
            id="deploy-attempts"
            label="Attempts before giving up"
            description="Until the next check. Each retry waits twice as long."
            min={1}
            max={10}
            value={deployAttempts}
            onChange={(value) => setDraft({ ...draft, deployAttempts: value })}
          />
          <NumberRow
            id="keep-versions"
            label="Versions kept per site"
            description="Older ones are forgotten once a day. The served and pinned versions are always kept. 0 keeps every version."
            min={0}
            max={1000}
            value={keepVersions}
            onChange={(value) => setDraft({ ...draft, keepVersions: value })}
          />
        </Section>
      )}

      {section.id === 'certificates' && status.data?.tls && (
        <>
          <TlsSummary status={status.data.tls} />
          <Section
            title="Certificate authority"
            footer="Setting HTTPS_PORT agrees to the certificate authority's terms of service. Try a new setup against Let's Encrypt's staging CA, whose certificates browsers don't trust, before it counts against the real one's limits."
          >
            <FieldRow label="ACME directory" htmlFor="acme-directory">
              <Input
                id="acme-directory"
                value={acmeDirectory}
                onChange={(event) => setDraft({ ...draft, acmeDirectory: event.target.value })}
                autoComplete="off"
                spellCheck={false}
                className="font-mono text-sm"
              />
            </FieldRow>
            {(acmeDirectory === LETS_ENCRYPT || acmeDirectory === LETS_ENCRYPT_STAGING) && (
              <ToggleRow
                id="acme-staging"
                label="Use the staging CA"
                description="Every site needs a new certificate when this changes."
                checked={acmeDirectory === LETS_ENCRYPT_STAGING}
                onCheckedChange={(staging) =>
                  setDraft({ ...draft, acmeDirectory: staging ? LETS_ENCRYPT_STAGING : LETS_ENCRYPT })
                }
              />
            )}
            <FieldRow label="Email" htmlFor="acme-email">
              <Input
                id="acme-email"
                type="email"
                value={acmeEmail}
                onChange={(event) => setDraft({ ...draft, acmeEmail: event.target.value })}
                placeholder="Nobody"
                autoComplete="off"
                spellCheck={false}
              />
              <p className="text-xs text-muted-foreground">Where the certificate authority writes about problems.</p>
            </FieldRow>
          </Section>
        </>
      )}

      {section.id === 'certificates' && (
        <Section
          title={status.data?.tls ? 'Limits' : undefined}
          footer={
            status.data?.tls
              ? "Caps how often npanel orders a certificate, so a misconfigured domain can't use up your certificate authority's limits."
              : "Caps how often npanel lets your proxy request a certificate, so a misconfigured domain can't use up your certificate authority's limits."
          }
        >
          <NumberRow
            id="tls-check-limit-per-host"
            label="Per domain"
            unit="an hour"
            min={1}
            value={tlsCheckLimitPerHost}
            onChange={(value) => setDraft({ ...draft, tlsCheckLimitPerHost: value })}
          />
          <NumberRow
            id="tls-check-limit-total"
            label="In total"
            unit="an hour"
            min={1}
            value={tlsCheckLimitTotal}
            onChange={(value) => setDraft({ ...draft, tlsCheckLimitTotal: value })}
          />
        </Section>
      )}

      {section.id === 'tor' && (
        <Section
          footer={
            current?.tor_enabled ? (
              <TorStatus status={status.data?.tor} />
            ) : (
              'Gives every site a .onion address. Tor is built into npanel, so there is nothing else to set up.'
            )
          }
        >
          <ToggleRow
            id="tor-enabled"
            label="Serve every site over Tor"
            checked={torEnabled}
            onCheckedChange={(checked) => setDraft({ ...draft, torEnabled: checked })}
          />
        </Section>
      )}

      {section.id === 'analytics' && (
        <Section footer="Where one day ends and the next begins, for every site.">
          <Row label="Time zone">
            <Select
              value={analyticsTimezone}
              onValueChange={(value) => setDraft({ ...draft, analyticsTimezone: value })}
            >
              <SelectTrigger className="w-56" aria-label="Time zone">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {timeZones(analyticsTimezone).map((zone) => (
                  <SelectItem key={zone} value={zone}>
                    {zone.replaceAll('_', ' ')}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </Row>
        </Section>
      )}

      {section.id === 'link-previews' && (
        <>
          <LinkPreviewCache />
          <RefreshLinkPreview />
          <Section
            title="Lookups"
            footer="Previews are looked up on a thread of their own, so a crawl never slows a site down. When a lookup can't start or takes too long, the crawler gets the page as it was published."
          >
            <NumberRow
              id="link-preview-ttl"
              label="Look up again after"
              unit="seconds"
              min={60}
              value={linkPreviewTtl}
              onChange={(value) => setDraft({ ...draft, linkPreviewTtl: value })}
            />
            <NumberRow
              id="link-preview-concurrency"
              label="Lookups at once"
              min={1}
              max={4096}
              value={linkPreviewConcurrency}
              onChange={(value) => setDraft({ ...draft, linkPreviewConcurrency: value })}
            />
            <NumberRow
              id="link-preview-misses"
              label="Relay lookups"
              description="Previews already cached don't count. This caps what someone making up links can make npanel ask relays for."
              unit="a minute"
              min={1}
              value={linkPreviewMissesPerMinute}
              onChange={(value) => setDraft({ ...draft, linkPreviewMissesPerMinute: value })}
            />
            <NumberRow
              id="link-preview-max-entries"
              label="Previews kept"
              description="The least recently looked up go first."
              min={100}
              value={linkPreviewMaxEntries}
              onChange={(value) => setDraft({ ...draft, linkPreviewMaxEntries: value })}
            />
          </Section>
          <Section footer="A case-insensitive regular expression, matched against the User-Agent. Everyone it doesn't match gets the page as published.">
            <FieldRow label="Crawlers" htmlFor="crawler-pattern">
              <Textarea
                id="crawler-pattern"
                value={crawlerPattern}
                onChange={(event) => setDraft({ ...draft, crawlerPattern: event.target.value })}
                rows={4}
                spellCheck={false}
                className="font-mono text-xs break-all"
              />
              {crawlerPattern !== settings.data?.defaults.link_preview_crawler_pattern && (
                <Button
                  variant="link"
                  size="sm"
                  className="h-auto px-0 text-xs"
                  onClick={() =>
                    setDraft({ ...draft, crawlerPattern: settings.data?.defaults.link_preview_crawler_pattern ?? '' })
                  }
                >
                  Use the default
                </Button>
              )}
            </FieldRow>
          </Section>
        </>
      )}

      {dirty && (
        <div className="fixed inset-x-0 bottom-20 z-20 flex justify-center px-4 md:bottom-6 md:pl-64">
          <div className="material flex items-center gap-3 rounded-full bg-popover py-2 pr-2 pl-5 shadow-xl ring-1 ring-black/10 dark:ring-white/10">
            <span className="text-sm">Unsaved changes</span>
            <Button variant="outline" size="sm" className="rounded-full" onClick={() => setDraft({})}>
              Revert
            </Button>
            <Button size="sm" className="rounded-full" onClick={save} disabled={updateSettings.isPending}>
              {updateSettings.isPending ? 'Saving…' : 'Save'}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}

/**
 * What the cache holds and how lookups have gone, with the one thing to do
 * about all of it: start over.
 */
function LinkPreviewCache() {
  const previews = useLinkPreviews();
  const clear = useClearLinkPreviews();
  const { toast } = useToast();

  const clearAll = async () => {
    try {
      const { cleared } = await clear.mutateAsync();
      toast({ title: 'Link previews cleared', description: `${cleared} ${cleared === 1 ? 'preview' : 'previews'} will be looked up again.` });
    } catch (error) {
      toast({
        title: "Couldn't clear link previews",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <Section
      title="Cache"
      footer="Turn link previews on for each site under its settings. Counts are since the server started."
      action={
        <AlertDialog>
          <AlertDialogTrigger asChild>
            <Button variant="ghost" size="sm" className="h-7 text-destructive hover:text-destructive" disabled={!previews.data || clear.isPending}>
              Clear cache…
            </Button>
          </AlertDialogTrigger>
          <AlertDialogContent size="sm">
            <AlertDialogHeader>
              <AlertDialogTitle>Clear every link preview?</AlertDialogTitle>
              <AlertDialogDescription>
                Each is looked up on relays again the next time a crawler asks for it.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction variant="destructive" onClick={clearAll}>
                Clear
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      }
    >
      {previews.isLoading ? (
        <Skeleton className="h-48 rounded-none" />
      ) : previews.error ? (
        <p className="px-4 py-3 text-sm text-muted-foreground">
          {previews.error instanceof Error ? previews.error.message : 'Unavailable.'}
        </p>
      ) : (
        previews.data && cacheRows(previews.data).map(([label, value]) => (
          <Row key={label} label={label}>
            <span className="text-sm text-muted-foreground tabular-nums">{value}</span>
          </Row>
        ))
      )}
    </Section>
  );
}

function cacheRows(status: LinkPreviewStatus): [string, string][] {
  const { stats } = status;
  const count = (n: number) => n.toLocaleString();
  return [
    ['Previews', count(status.found)],
    ['Kinds with no preview', count(status.unsupported)],
    ['Found nothing', count(status.not_found)],
    ['Disk used', formatBytes(status.bytes)],
    ['Answered from memory', count(stats.hits)],
    ['Looked up', count(stats.lookups)],
    ['Fetched from relays', count(stats.fetches)],
    ['Sent without a preview', count(stats.busy + stats.timeouts)],
    ['Relay lookups held back', count(stats.over_budget)],
  ];
}

/** Look one entity up again, and show what crawlers will now be given. */
function RefreshLinkPreview() {
  const [entity, setEntity] = useState('');
  const [result, setResult] = useState<RefreshedLinkPreview>();
  const refresh = useRefreshLinkPreview();
  const { toast } = useToast();

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!entity.trim()) return;
    try {
      setResult(await refresh.mutateAsync(entity.trim()));
    } catch (error) {
      setResult(undefined);
      toast({
        title: "Couldn't refresh that preview",
        description: error instanceof Error ? error.message : String(error),
        variant: 'destructive',
      });
    }
  };

  return (
    <Section
      title="Refresh a preview"
      footer="When a post or profile was edited and a crawler still shows the old version. Paste an npub, a note, an naddr, a NIP-05 name, or a link to a page."
    >
      <form onSubmit={submit} className="flex gap-2 py-3 pr-4 pl-4">
        <Input
          aria-label="What to refresh"
          value={entity}
          onChange={(event) => setEntity(event.target.value)}
          placeholder="nevent1…, alex@example.com, https://…"
          autoComplete="off"
          spellCheck={false}
          className="font-mono text-sm"
        />
        <Button type="submit" disabled={!entity.trim() || refresh.isPending} className="shrink-0">
          {refresh.isPending ? 'Looking up…' : 'Refresh'}
        </Button>
      </form>
      {result && <RefreshedPreview result={result} />}
    </Section>
  );
}

/** A refreshed preview, drawn roughly as a chat app would unfurl it. */
function RefreshedPreview({ result }: { result: RefreshedLinkPreview }) {
  const { card } = result;

  if (result.unsupported !== null) {
    return (
      <Row
        label="No preview"
        description={`${result.entity} is a kind ${result.unsupported} event, which has no link preview. Crawlers get the page as published.`}
      />
    );
  }

  if (!card) {
    return (
      <Row label="Nothing found" description={`No relay had ${result.entity}. Crawlers get the page as published.`} />
    );
  }

  const author = card.author.name ?? `${card.author.npub.slice(0, 12)}…`;
  const image = card.image ?? card.author.picture;

  return (
    <div className="pl-4">
      <div data-row className="flex gap-3 border-t py-3 pr-4">
        {image && (
          <img
            src={image}
            alt=""
            className={cn('shrink-0 rounded-md bg-muted object-cover', card.image ? 'h-20 w-32' : 'size-12 rounded-full')}
            referrerPolicy="no-referrer"
          />
        )}
        <div className="min-w-0 space-y-1">
          <p className="text-sm font-medium break-words">{card.title ?? author}</p>
          {card.title && <p className="text-xs text-muted-foreground">{author}</p>}
          {card.description && <p className="line-clamp-3 text-xs text-muted-foreground break-words">{card.description}</p>}
          <p className="font-mono text-[11px] text-muted-foreground/80 break-all">{result.entity}</p>
        </div>
      </div>
    </div>
  );
}

function NumberRow({
  id,
  label,
  description,
  unit,
  value,
  onChange,
  min,
  max,
}: {
  id: string;
  label: string;
  description?: string;
  unit?: string;
  value: string;
  onChange(value: string): void;
  min?: number;
  max?: number;
}) {
  return (
    <Row label={label} description={description} htmlFor={id}>
      <span className="flex items-center gap-2 text-sm text-muted-foreground">
        <Input
          id={id}
          type="number"
          min={min}
          max={max}
          value={value}
          onChange={(event) => onChange(event.target.value)}
          className="h-8 w-24 text-right tabular-nums"
        />
        {unit}
      </span>
    </Row>
  );
}

/**
 * How npanel's own certificates stand, and which names can't get one: the
 * question to answer before anyone asks why a site won't load.
 */
function TlsSummary({ status }: { status: TlsStatusView }) {
  return (
    <Section
      title="HTTPS"
      footer={`${status.issued} of ${status.names} ${status.names === 1 ? 'domain has' : 'domains have'} a certificate. npanel gets them itself and renews them before they expire.`}
    >
      {status.failing.length ? (
        status.failing.map((certificate) => <CertificateRow key={certificate.hostname} certificate={certificate} />)
      ) : (
        <Row label="Nothing is failing" />
      )}
    </Section>
  );
}

/** Whether npanel is actually reaching Tor, which only it can tell you. */
function TorStatus({ status }: { status: TorStatusView | undefined }) {
  if (!status) return null;

  if (status.connected) {
    return (
      <span className="flex items-center gap-1.5">
        <span className="size-2 rounded-full bg-success" aria-hidden />
        Connected · {status.services} {status.services === 1 ? 'site' : 'sites'} on Tor
      </span>
    );
  }

  if (!status.error) {
    return (
      <span className="flex items-center gap-1.5">
        <span className="size-2 rounded-full bg-muted-foreground/50" aria-hidden />
        Joining the Tor network · {status.progress}%
      </span>
    );
  }

  return (
    <span className="flex items-start gap-1.5">
      <span className="mt-1 size-2 shrink-0 rounded-full bg-destructive" aria-hidden />
      <span>{status.error} npanel keeps trying, so fixing this needs no restart.</span>
    </span>
  );
}

function ContactPreview({ value }: { value: string }) {
  const pubkey = decodePubkey(value);

  if (!value.trim()) return <p className="text-xs text-muted-foreground">Nobody is named.</p>;
  if (!pubkey) return <p className="text-xs text-destructive">That isn't an npub.</p>;

  return (
    <p className="flex items-center gap-2 text-xs text-muted-foreground">
      Shown as <PubkeyLabel pubkey={pubkey} className="text-foreground" />
    </p>
  );
}

/**
 * Every time zone the browser knows, UTC first.
 *
 * The browser's list rather than one of our own, since it is the same IANA
 * data the server checks against. UTC is added because some browsers leave it
 * out of the list, and the current value in case this browser is older than
 * the zone it names.
 */
function timeZones(current: string): string[] {
  const zones = new Set(['UTC', current, ...Intl.supportedValuesOf('timeZone')]);
  return [...zones];
}

function lines(value: string): string[] {
  return value
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean);
}

export default SettingsSectionPage;
