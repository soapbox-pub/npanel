import type { Host } from '@/lib/api-types';

export type Tone = 'green' | 'blue' | 'orange' | 'red' | 'gray';

/** What a site is doing, in the sync worker's terms but in a word or two. */
export function siteStatus(host: Host): { tone: Tone; label: string } {
  if (!host.enabled) return { tone: 'gray', label: 'Off' };

  const latest = host.latest;
  if (!latest) return { tone: 'gray', label: 'Waiting for the nsite' };
  if (latest.status === 'failed') return { tone: 'red', label: host.active ? 'Update failed' : 'Failed' };

  if (latest.status !== 'ready') {
    const progress = latest.files_total ? Math.round((latest.files_ready / latest.files_total) * 100) : 0;
    return { tone: 'blue', label: `Deploying ${progress}%` };
  }

  if (host.pinned_event_id) return { tone: 'orange', label: 'Pinned' };
  return { tone: 'green', label: 'Live' };
}
