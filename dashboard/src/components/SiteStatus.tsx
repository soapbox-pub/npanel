import type { Host } from '@/lib/api-types';
import { type Tone, siteStatus } from '@/lib/site-status';
import { cn } from '@/lib/utils';

const DOTS: Record<Tone, string> = {
  green: 'bg-success',
  blue: 'bg-primary animate-pulse',
  orange: 'bg-warning',
  red: 'bg-destructive',
  gray: 'bg-muted-foreground/50',
};

/** A site's status as a coloured dot and its word, the way Finder shows a tag. */
export function SiteStatus({ host, className }: { host: Host; className?: string }) {
  const { tone, label } = siteStatus(host);

  return (
    <span className={cn('inline-flex items-center gap-1.5 text-sm text-muted-foreground', className)}>
      <span className={cn('size-2 shrink-0 rounded-full', DOTS[tone])} aria-hidden />
      {label}
    </span>
  );
}
