import type { ReactNode } from 'react';

import { SOURCE_URL } from '@/lib/links';
import { cn } from '@/lib/utils';

/**
 * A row of 88×31 buttons, the badges every homepage of the early web wore in
 * its footer to say what it was made with. Drawn rather than served as GIFs,
 * so they stay crisp and need nothing from anywhere else.
 */
export function WebButtons({ className }: { className?: string }) {
  return (
    <div className={cn('flex flex-wrap gap-1.5', className)}>
      <a href={SOURCE_URL} target="_blank" rel="noreferrer noopener" aria-label="npanel source code">
        <Badge className="bg-linear-to-b from-[#ff9a2e] to-[#f9541a]">
          <span className="flex flex-col items-center leading-none">
            <span className="text-[7px] font-bold tracking-widest uppercase">hosted by</span>
            <span className="text-[13px] font-black tracking-tight">npanel</span>
          </span>
        </Badge>
      </a>
      <Badge className="bg-[#5b2a86]" label="Nostr">
        <span className="text-[#e6c6ff]">⚡</span>
        <span className="text-[11px] font-black tracking-widest">NOSTR</span>
      </Badge>
      <Badge className="bg-[#101010]" label="nsite ready">
        <span className="bg-[#5dff7a] px-0.5 text-[9px] font-bold text-black">nsite</span>
        <span className="text-[9px] font-bold text-[#5dff7a]">READY</span>
      </Badge>
      <Badge className="bg-linear-to-b from-[#fbf0df] to-[#f3d9b1] text-[#3b2a1a]" label="Made with Bun">
        <span className="text-[9px] font-bold">made with</span>
        <span className="text-[12px] leading-none font-black">Bun</span>
      </Badge>
      <Badge className="bg-[#003b57]" label="SQLite">
        <span className="text-[11px] font-bold italic">SQLite</span>
        <Tag>db</Tag>
      </Badge>
      <Badge className="bg-[#c0c0c0] text-black" label="Best viewed with any browser">
        <span className="text-center text-[8px] leading-[9px] font-bold">
          BEST VIEWED
          <br />
          ANY BROWSER
        </span>
      </Badge>
    </div>
  );
}

/**
 * One button: 88 by 31, a bevelled edge lit from the top left, and pixels
 * left square when scaled, as a GIF's would be.
 */
function Badge({ className, label, children }: { className?: string; label?: string; children: ReactNode }) {
  return (
    <span
      role={label ? 'img' : undefined}
      aria-label={label}
      className={cn(
        'flex h-[31px] w-[88px] shrink-0 items-center justify-center gap-1 overflow-hidden font-mono text-white select-none',
        'border border-t-white/60 border-r-black/60 border-b-black/60 border-l-white/60',
        className,
      )}
    >
      {children}
    </span>
  );
}

function Tag({ children }: { children: ReactNode }) {
  return <span className="bg-white px-0.5 text-[8px] font-bold text-black uppercase">{children}</span>;
}
