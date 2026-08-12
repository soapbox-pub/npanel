import { Badge } from '@/components/ui/badge';
import type { Host } from '@/lib/api-types';

/** One-glance deploy state for a host, mirroring the sync worker's statuses. */
export function StatusBadge({ host }: { host: Host }) {
  if (!host.enabled) {
    return <Badge variant="outline">Disabled</Badge>;
  }

  const latest = host.latest;

  if (!latest) {
    return <Badge variant="outline">Waiting for manifest</Badge>;
  }

  if (latest.status === 'failed' && !host.active) {
    return <Badge variant="destructive">Failed</Badge>;
  }

  if (latest.status === 'failed') {
    return <Badge variant="destructive">Update failed</Badge>;
  }

  if (latest.status !== 'ready') {
    const progress = latest.files_total ? Math.round((latest.files_ready / latest.files_total) * 100) : 0;
    return <Badge variant="secondary">Deploying {progress}%</Badge>;
  }

  if (host.pinned_event_id) {
    return <Badge variant="secondary">Pinned</Badge>;
  }

  return <Badge>Live</Badge>;
}
