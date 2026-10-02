import { useSeoMeta } from '@unhead/react';
import { useState } from 'react';
import { useParams } from 'react-router-dom';

import { FieldRow, Row, RowIcon, Section, ToggleRow } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Textarea } from '@/components/ui/textarea';
import { useDnsTarget, useSettings, useStatus, useUpdateSettings } from '@/hooks/useNpanel';
import { useToast } from '@/hooks/useToast';
import type { TorStatus as TorStatusView } from '@/lib/api-types';
import { decodePubkey } from '@/lib/format';
import { findSettingsSection } from '@/lib/settings-sections';
import { cn } from '@/lib/utils';
import NotFound from '@/pages/NotFound';

interface Draft {
  relays?: string;
  servers?: string;
  maxFileSize?: string;
  maxSiteSize?: string;
  maxSiteFiles?: string;
  deployAttempts?: string;
  resyncInterval?: string;
  allowedHostSuffixes?: string;
  dnsTargets?: string;
  tlsCheckLimitPerHost?: string;
  tlsCheckLimitTotal?: string;
  contactNpub?: string;
  publicSiteList?: boolean;
  torEnabled?: boolean;
  analyticsTimezone?: string;
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
  const allowedHostSuffixes = draft.allowedHostSuffixes ?? current?.allowed_host_suffixes.join('\n') ?? '';
  const dnsTargets = draft.dnsTargets ?? current?.dns_targets.join('\n') ?? '';
  const tlsCheckLimitPerHost = draft.tlsCheckLimitPerHost ?? String(current?.tls_check_limit_per_host ?? '');
  const tlsCheckLimitTotal = draft.tlsCheckLimitTotal ?? String(current?.tls_check_limit_total ?? '');
  const contactNpub = draft.contactNpub ?? current?.contact_npub ?? '';
  const publicSiteList = draft.publicSiteList ?? current?.public_site_list ?? false;
  const torEnabled = draft.torEnabled ?? current?.tor_enabled ?? false;
  const analyticsTimezone = draft.analyticsTimezone ?? current?.analytics_timezone ?? 'UTC';

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
        allowed_host_suffixes: lines(allowedHostSuffixes),
        dns_targets: lines(dnsTargets),
        tls_check_limit_per_host: Number(tlsCheckLimitPerHost),
        tls_check_limit_total: Number(tlsCheckLimitTotal),
        contact_npub: contactNpub.trim(),
        public_site_list: publicSiteList,
        tor_enabled: torEnabled,
        analytics_timezone: analyticsTimezone,
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
        </Section>
      )}

      {section.id === 'certificates' && (
        <Section footer="Caps how often npanel lets your proxy request a certificate, so a misconfigured domain can't use up your certificate authority's limits.">
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
