import { useState } from 'react';

import { siteTile } from '@/lib/site-tile';
import { cn } from '@/lib/utils';

/**
 * A site's own icon on a rounded square, as a home screen shows one, and a
 * monogram tile when it has none — or when the one it names won't load.
 */
export function SiteIcon({ hostname, src, className }: { hostname: string; src?: string | null; className?: string }) {
  // Remembered by URL, so a site that later publishes a working icon gets it.
  const [failed, setFailed] = useState<string | null>(null);
  const icon = src && failed !== src ? src : null;
  const letter = hostname.replace(/^www\./, '').charAt(0).toUpperCase();

  return (
    <span
      aria-hidden
      className={cn(
        '@container relative flex size-8 shrink-0 items-center justify-center overflow-hidden rounded-[22%] font-semibold text-white shadow-[inset_0_0_0_0.5px_rgb(0_0_0/0.08)]',
        !icon && cn('bg-linear-to-b', siteTile(hostname)),
        className,
      )}
    >
      {icon ? (
        <>
          {/*
           * White for icons with transparency, stopping a pixel short of the
           * edge. Were it the tile's own background, the rounded corners would
           * blend it through the icon's antialiased edge as a light fringe.
           */}
          <span className="absolute inset-px rounded-[22%] bg-white" />
          <img
            src={icon}
            alt=""
            loading="lazy"
            onError={() => setFailed(icon)}
            className="relative size-full object-cover"
          />
        </>
      ) : (
        <span className="text-[55cqw] leading-none">{letter}</span>
      )}
    </span>
  );
}
