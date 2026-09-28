import { useSeoMeta } from '@unhead/react';

import { Section } from '@/components/Grouped';
import { PageHeader } from '@/components/PageHeader';
import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Skeleton } from '@/components/ui/skeleton';
import { useAuditLog } from '@/hooks/useNpanel';
import { fullDate, timeAgo } from '@/lib/format';

/** Every configuration change and who made it, most recent first. Admin only. */
export function AuditLogPage() {
  useSeoMeta({ title: 'Activity — npanel' });

  const entries = useAuditLog();

  return (
    <div className="space-y-8">
      <PageHeader title="Activity" />

      {entries.isLoading ? (
        <Skeleton className="h-64 w-full rounded-xl" />
      ) : !entries.data?.length ? (
        <p className="py-16 text-center text-muted-foreground">Nothing has changed yet.</p>
      ) : (
        <Section>
          {entries.data.map((entry) => (
            <div key={entry.id} className="pl-4">
              <div data-row className="flex items-start gap-3 border-t py-3 pr-4">
                <div className="min-w-0 flex-1 space-y-0.5">
                  <p className="flex min-w-0 flex-wrap items-baseline gap-x-1.5 text-sm">
                    <PubkeyLabel pubkey={entry.actor_pubkey} className="font-medium" />
                    <span className="text-muted-foreground">{entry.action.replaceAll('_', ' ').replaceAll('.', ' ')}</span>
                    {entry.target && <span className="truncate font-medium">{entry.target}</span>}
                  </p>
                  <p className="truncate font-mono text-xs text-muted-foreground">
                    {change(entry.old_value, entry.new_value)}
                  </p>
                </div>
                <time
                  dateTime={new Date(entry.created_at * 1000).toISOString()}
                  title={fullDate(entry.created_at)}
                  className="shrink-0 text-xs text-muted-foreground"
                >
                  {timeAgo(entry.created_at)}
                </time>
              </div>
            </div>
          ))}
        </Section>
      )}
    </div>
  );
}

/** What changed: the new value alone for something created, both for an edit. */
function change(before: unknown, after: unknown): string {
  if (before === null || before === undefined) return format(after);
  return `${format(before)} → ${format(after)}`;
}

function format(value: unknown): string {
  if (value === null || value === undefined) return '—';
  return typeof value === 'string' ? value : JSON.stringify(value);
}

export default AuditLogPage;
