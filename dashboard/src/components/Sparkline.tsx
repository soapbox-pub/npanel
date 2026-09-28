import { useId } from 'react';

import { cn } from '@/lib/utils';

const WIDTH = 480;
const HEIGHT = 80;

/**
 * A day of visitors as a line, the way Plausible draws one on each site in
 * its list: straight segments, the peak hour touching the top, and a wash of
 * the line's colour beneath it. It takes the colour of its text.
 */
export function Sparkline({ values, className }: { values: number[]; className?: string }) {
  const id = useId();
  const max = Math.max(...values, 0);
  const step = values.length > 1 ? WIDTH / (values.length - 1) : WIDTH;

  const points = values.map((value, index) => {
    const y = max ? HEIGHT - (value / max) * HEIGHT : HEIGHT;
    return `${(index * step).toFixed(1)},${y.toFixed(1)}`;
  });
  const area = [`0,${HEIGHT}`, ...points, `${WIDTH},${HEIGHT}`].join(' ');

  return (
    <svg
      viewBox={`0 -2 ${WIDTH} ${HEIGHT + 4}`}
      preserveAspectRatio="none"
      className={cn('w-full overflow-visible', className)}
      aria-hidden
    >
      <defs>
        <linearGradient id={id} x1="0" x2="0" y1="0" y2="1">
          <stop offset="0%" stopColor="currentColor" stopOpacity={0.2} />
          <stop offset="100%" stopColor="currentColor" stopOpacity={0} />
        </linearGradient>
      </defs>
      <polygon points={area} fill={`url(#${id})`} />
      <polyline
        points={points.join(' ')}
        fill="none"
        stroke="currentColor"
        strokeWidth={2}
        strokeLinejoin="round"
        strokeLinecap="round"
        vectorEffect="non-scaling-stroke"
      />
    </svg>
  );
}
