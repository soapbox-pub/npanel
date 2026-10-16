import type { Tunnel } from '@/lib/api-types';
import { timeAgo } from '@/lib/format';
import type { TileColor } from '@/lib/tile-colors';

/** Where a tunnel stands, as the list and its own page both say it. */
export interface TunnelStatus {
  label: string;
  /** Why nothing is connected, when nothing is. */
  detail?: string;
  color: TileColor;
}

export function tunnelStatus(tunnel: Tunnel): TunnelStatus {
  if (!tunnel.enabled) {
    return { label: 'Off', detail: 'Turned off here. The agent waits to be turned back on.', color: 'gray' };
  }
  if (tunnel.connections.length) {
    return { label: 'Connected', color: 'green' };
  }
  return {
    label: 'Offline',
    detail: tunnel.connected_at
      ? `Last connected ${timeAgo(tunnel.connected_at)}. Visitors get an error until the agent is running again.`
      : "The agent hasn't connected yet.",
    color: 'red',
  };
}
