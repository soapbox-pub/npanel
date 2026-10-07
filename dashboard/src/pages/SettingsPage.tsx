import { useSeoMeta } from '@unhead/react';

import { LinkRow, Row, RowIcon, Section } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { Skeleton } from '@/components/ui/skeleton';
import { useSettings, useStatus, useStorage } from '@/hooks/useNpanel';
import type { Settings, Storage } from '@/lib/api-types';
import { formatBytes, timeAgo } from '@/lib/format';
import { SETTINGS_SECTIONS, type SettingsSectionId } from '@/lib/settings-sections';

/**
 * The gateway's settings, stored in the database rather than the environment:
 * what the server is doing, then a page for each part of its configuration.
 */
export function SettingsPage() {
  useSeoMeta({ title: 'Settings — npanel' });

  const settings = useSettings();
  const current = settings.data?.settings;

  return (
    <div className="space-y-8">
      <PageHeader title="Settings" />

      <ServerInformation />

      <div className="space-y-6">
        {SETTINGS_SECTIONS.map((group) => (
          <Section key={group[0].id}>
            {group.map((section) => (
              <LinkRow
                key={section.id}
                to={`/settings/${section.id}`}
                icon={
                  <RowIcon color={section.color}>
                    <section.icon />
                  </RowIcon>
                }
                label={section.label}
                value={current && summary(section.id, current)}
              />
            ))}
          </Section>
        ))}
      </div>
    </div>
  );
}

/** A word or two of what a section holds, beside its name, where there's one worth saying. */
function summary(id: SettingsSectionId, settings: Settings): string | undefined {
  switch (id) {
    case 'homepage':
      return settings.public_site_list ? 'Sites listed' : undefined;
    case 'domains':
      return settings.allowed_host_suffixes.length
        ? `${settings.allowed_host_suffixes.length} allowed`
        : 'Any domain';
    case 'nostr':
      return `${settings.relays.length} ${settings.relays.length === 1 ? 'relay' : 'relays'}`;
    case 'deploys':
      return settings.keep_versions ? `Keeps ${settings.keep_versions} ${settings.keep_versions === 1 ? 'version' : 'versions'}` : 'Keeps every version';
    case 'tor':
      return settings.tor_enabled ? 'On' : 'Off';
    case 'analytics':
      return settings.analytics_timezone.replaceAll('_', ' ');
    case 'errors':
      return settings.errors_retention_days === undefined ? undefined : `Kept ${settings.errors_retention_days} days`;
    default:
      return undefined;
  }
}

/**
 * The readout every control panel has, answering the question staff ask
 * first: is this box doing anything?
 */
function ServerInformation() {
  const status = useStatus();
  const storage = useStorage();
  const rows: [string, string][] = status.data
    ? [
        ['Sites', String(status.data.hosts)],
        ['Deployed versions', String(status.data.ready_versions)],
        ['Stored files', String(status.data.blobs)],
        ['Disk used', formatBytes(status.data.blob_bytes)],
        ...(storage.data ? cleanup(storage.data) : []),
        ['Relays', String(status.data.relays.length)],
      ]
    : [];

  return (
    <Section title="Server Information">
      {status.isLoading ? (
        <Skeleton className="h-60 rounded-none" />
      ) : !rows.length ? (
        <p className="px-4 py-3 text-sm text-muted-foreground">Unavailable.</p>
      ) : (
        rows.map(([label, value]) => (
          <Row key={label} label={label}>
            <span className="text-sm text-muted-foreground tabular-nums">{value}</span>
          </Row>
        ))
      )}
    </Section>
  );
}

/** What the daily prune has freed, and has yet to. */
function cleanup(storage: Storage): [string, string][] {
  const rows: [string, string][] = [['Waiting to be cleaned up', formatBytes(storage.unreferenced.bytes)]];
  const last = storage.last_prune;
  if (last) {
    rows.push(['Last cleaned up', `${timeAgo(last.finished_at)}, freed ${formatBytes(last.bytes)}`]);
  }
  return rows;
}

export default SettingsPage;
