import { formatBytes } from '@/lib/format';
import { useStatus } from '@/hooks/useNpanel';

/**
 * The rail's server readout.
 *
 * Every control panel has one of these, and it answers the question staff
 * actually ask first: is this box doing anything?
 */
export function ServerInfo() {
  const status = useStatus();

  const rows: [string, string][] = status.data
    ? [
        ['Sites', String(status.data.hosts)],
        ['Deployed', String(status.data.ready_versions)],
        ['Blobs', String(status.data.blobs)],
        ['On disk', formatBytes(status.data.blob_bytes)],
        ['Relays', String(status.data.relays.length)],
      ]
    : [];

  return (
    <div className="px-3 py-4">
      <h2 className="px-2 pb-2 font-mono text-[10px] font-semibold uppercase tracking-widest text-sidebar-foreground/50">
        Server information
      </h2>
      <dl className="space-y-1.5 rounded-md bg-black/25 p-3">
        {rows.length === 0 ? (
          <div className="text-xs text-sidebar-foreground/50">
            {status.isError ? 'Unavailable.' : 'Loading…'}
          </div>
        ) : (
          rows.map(([label, value]) => (
            <div key={label} className="flex items-baseline justify-between gap-3">
              <dt className="text-xs text-sidebar-foreground/70">{label}</dt>
              <dd className="font-mono text-xs font-medium text-sidebar-accent-foreground">{value}</dd>
            </div>
          ))
        )}
      </dl>
    </div>
  );
}
