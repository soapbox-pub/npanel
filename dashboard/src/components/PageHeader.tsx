import { ChevronLeft } from 'lucide-react';
import type { ReactNode } from 'react';
import { Link } from 'react-router-dom';

import { cn } from '@/lib/utils';

/**
 * A page's large title, as iOS and macOS put one at the top of a view: a
 * back link above it when the page is a level down, and the page's own
 * actions beside it.
 */
export function PageHeader({
  title,
  subtitle,
  back,
  banner,
  icon,
  actions,
  className,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  back?: { to: string; label: string };
  /** A picture above the title, faded at its foot; the title overlaps it where it shows. */
  banner?: ReactNode;
  /** Drawn in front of the title, like a {@link SiteIcon}. */
  icon?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <header className={cn('space-y-3', className)}>
      {back && (
        <Link
          to={back.to}
          className="-ml-1.5 inline-flex max-w-full items-center text-sm text-primary hover:opacity-80 focus-visible:rounded-md focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden"
        >
          <ChevronLeft className="size-5 shrink-0" aria-hidden />
          <span className="truncate">{back.label}</span>
        </Link>
      )}

      {banner}

      <div
        className={cn(
          'flex flex-wrap items-center justify-between gap-x-6 gap-y-4',
          // Pulled up into the banner's fade, so the two read as one.
          banner && 'relative sm:-mt-12 sm:px-6',
        )}
      >
        <div className="flex min-w-0 items-center gap-4">
          {icon}
          <div className="min-w-0">
            <h1 className="truncate text-[28px] leading-tight font-bold tracking-tight sm:text-[34px]">{title}</h1>
            {subtitle && <div className="mt-0.5 text-sm text-muted-foreground">{subtitle}</div>}
          </div>
        </div>
        {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
      </div>
    </header>
  );
}
