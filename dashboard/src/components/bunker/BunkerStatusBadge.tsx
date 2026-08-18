import { Badge } from '@/components/ui/badge';
import type { BunkerStatus } from '@/lib/api-types';

/**
 * Whether a bunker can reach the signer that holds its key.
 *
 * `error` is shown beside the badge rather than hidden in a tooltip: it says
 * what the signer answered, which is the only thing that tells you whether to
 * unlock a phone or pair again.
 */
export function BunkerStatusBadge({ status, error }: { status: BunkerStatus; error: string | null }) {
  if (status === 'connected') {
    return <Badge variant="secondary">Connected</Badge>;
  }

  if (status === 'pending') {
    return <Badge variant="outline">Not paired</Badge>;
  }

  return (
    <span className="flex flex-wrap items-center gap-2">
      <Badge variant="destructive">Not answering</Badge>
      {error && <span className="text-xs text-muted-foreground">{error}</span>}
    </span>
  );
}
