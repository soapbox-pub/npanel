import { useId } from 'react';

import { cn } from '@/lib/utils';

/**
 * The npanel mark: a rounded tile with a geometric "n".
 *
 * The glyph is drawn rather than typeset so it renders identically here and in
 * `public/favicon.svg`, which is the same artwork. Keep the two in sync.
 */
export function Logo({ className }: { className?: string }) {
  const gradient = useId();

  return (
    <svg viewBox="0 0 64 64" className={cn('size-8', className)} role="img" aria-label="npanel">
      <defs>
        <linearGradient id={gradient} x1="0" y1="0" x2="1" y2="1">
          <stop offset="0" stopColor="#ff9a2e" />
          <stop offset="1" stopColor="#f9541a" />
        </linearGradient>
      </defs>
      <rect width="64" height="64" rx="14" fill={`url(#${gradient})`} />
      <g fill="none" stroke="#fff" strokeWidth="8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M21 45V20" />
        <path d="M21 31a11 11 0 0 1 22 0v14" />
      </g>
    </svg>
  );
}
