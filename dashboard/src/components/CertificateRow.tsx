import { Row } from '@/components/Grouped';
import type { CertificateStatus } from '@/lib/api-types';
import { fullDate, timeAgo } from '@/lib/format';
import { cn } from '@/lib/utils';

/**
 * One name's certificate, when npanel gets certificates itself: whether
 * there is one, until when, and what went wrong if anything did.
 */
export function CertificateRow({ certificate }: { certificate: CertificateStatus }) {
  const { expires_at: expires, error } = certificate;
  const tried = certificate.attempted_at ? ` Last tried ${timeAgo(certificate.attempted_at)}.` : '';

  let description: string;
  if (expires && error) {
    description = `Valid until ${fullDate(expires)}, but renewing it failed: ${error}.${tried} npanel keeps trying.`;
  } else if (expires) {
    description = `Valid until ${fullDate(expires)}. Renewed on its own.`;
  } else if (error) {
    description = `No certificate yet: ${error}.${tried} npanel keeps trying, and waits longer after each failure.`;
  } else {
    description = 'Getting a certificate…';
  }

  return (
    <Row
      label={
        <span className="flex items-center gap-1.5">
          <span
            className={cn(
              'size-2 shrink-0 rounded-full',
              error ? 'bg-destructive' : expires ? 'bg-success' : 'bg-muted-foreground/50',
            )}
            aria-hidden
          />
          <span className="truncate font-mono">{certificate.hostname}</span>
        </span>
      }
      description={description}
    />
  );
}
