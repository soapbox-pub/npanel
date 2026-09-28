import { useState } from 'react';

import { cn } from '@/lib/utils';

/** Gradients a site without a favicon is drawn in, picked by its name so it keeps one. */
const TILES = [
  'from-[#64d2ff] to-[#0a84ff]',
  'from-[#ffd60a] to-[#ff9f0a]',
  'from-[#ff9f0a] to-[#ff453a]',
  'from-[#30d158] to-[#00a86b]',
  'from-[#da8fff] to-[#bf5af2]',
  'from-[#7d7aff] to-[#5e5ce6]',
  'from-[#ff6482] to-[#ff2d55]',
  'from-[#6ac4dc] to-[#30b0c7]',
];

/**
 * A site's own favicon, as Safari shows one, and a monogram tile when the
 * site has none: the way a list of sites is told apart at a glance.
 */
export function SiteIcon({ hostname, className }: { hostname: string; className?: string }) {
  const [broken, setBroken] = useState(false);

  let hash = 0;
  for (const char of hostname) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  const letter = hostname.replace(/^www\./, '').charAt(0).toUpperCase();

  return (
    <span
      aria-hidden
      className={cn(
        '@container relative flex size-8 shrink-0 items-center justify-center overflow-hidden rounded-[22%] font-semibold text-white shadow-[inset_0_0_0_0.5px_rgb(0_0_0/0.08)]',
        broken ? cn('bg-linear-to-b', TILES[hash % TILES.length]) : 'bg-card',
        className,
      )}
    >
      {broken ? (
        <span className="text-[55cqw] leading-none">{letter}</span>
      ) : (
        <img
          src={`https://${hostname}/favicon.ico`}
          alt=""
          loading="lazy"
          onError={() => setBroken(true)}
          className="size-full object-contain"
        />
      )}
    </span>
  );
}
