import { Link } from 'react-router-dom';

import { useHosts, useStatus } from '@/hooks/useNpanel';
import { formatBytes } from '@/lib/format';
import { type Tone, siteStatus } from '@/lib/site-status';
import { cn } from '@/lib/utils';

/** Drive bays on the faceplate. A busier gateway fills them; the rest sit dark. */
const BAYS = 8;

const BAY_LIGHTS: Record<Tone, string> = {
  green: 'bg-[#30d158] shadow-[0_0_4px_#30d158]',
  blue: 'bg-[#64d2ff] shadow-[0_0_4px_#64d2ff] motion-safe:animate-[rack-blink_0.6s_steps(2)_infinite]',
  orange: 'bg-[#ff9f0a] shadow-[0_0_4px_#ff9f0a]',
  red: 'bg-[#ff453a] shadow-[0_0_4px_#ff453a] motion-safe:animate-[rack-blink_1.2s_steps(2)_infinite]',
  gray: 'bg-white/15',
};

/**
 * The box this all runs on, drawn as a 1U rackmount server: rack ears, a
 * vent grille, a green-phosphor readout, and a drive bay per site whose light
 * is that site's status. A nod to when hosting a website meant a machine in a
 * cupboard somewhere, blinking — and still a glanceable answer to whether
 * anything is wrong.
 */
export function ServerRack({ className }: { className?: string }) {
  const status = useStatus();
  const hosts = useHosts();

  const sites = hosts.data?.hosts ?? [];
  const bays = Array.from({ length: BAYS }, (_, index) => sites[index]);
  const readout = status.data
    ? `${status.data.hosts} ${status.data.hosts === 1 ? 'SITE' : 'SITES'} ${formatBytes(status.data.blob_bytes)}`
    : 'BOOTING…';

  return (
    <Link
      to="/"
      aria-label={`Server: ${readout.toLowerCase()}`}
      className={cn(
        'group relative flex h-11 items-center gap-2 rounded-[5px] bg-linear-to-b from-[#3a3a3e] via-[#2a2a2d] to-[#1f1f22] px-1.5 text-white shadow-[inset_0_1px_0_rgb(255_255_255/0.12),inset_0_-1px_0_rgb(0_0_0/0.4),0_1px_2px_rgb(0_0_0/0.3)] focus-visible:ring-[3px] focus-visible:ring-ring/40 focus-visible:outline-hidden',
        className,
      )}
    >
      <Ear />

      <div className="flex shrink-0 flex-col gap-1" aria-hidden>
        <span className="size-1.5 rounded-full bg-[#30d158] shadow-[0_0_4px_#30d158]" title="Power" />
        <span className="size-1.5 rounded-full bg-[#ff9f0a] shadow-[0_0_4px_#ff9f0a] motion-safe:animate-[rack-blink_0.9s_steps(2)_infinite]" />
      </div>

      <div className="min-w-0 flex-1 space-y-1">
        <div className="truncate rounded-[2px] bg-[#0b1a0b] px-1 font-mono text-[9px] leading-[14px] tracking-wider text-[#5dff7a] shadow-[inset_0_1px_2px_rgb(0_0_0/0.8)] [text-shadow:0_0_4px_rgb(93_255_122/0.7)]">
          {readout}
        </div>
        <div className="flex gap-[3px]" aria-hidden>
          {bays.map((host, index) => (
            <span
              // Bays are positions, not sites: a site moving up a slot is the same bay lighting.
              key={index}
              className="flex h-1.5 flex-1 items-center justify-end rounded-[1px] bg-black/40 px-px shadow-[inset_0_1px_0_rgb(0_0_0/0.5)]"
            >
              <span className={cn('size-[3px] rounded-full', BAY_LIGHTS[host ? siteStatus(host).tone : 'gray'])} />
            </span>
          ))}
        </div>
      </div>

      <div
        aria-hidden
        className="h-7 w-5 shrink-0 rounded-[2px] bg-[repeating-linear-gradient(to_bottom,rgb(0_0_0/0.55)_0_1.5px,transparent_1.5px_3.5px)]"
      />

      <Ear />
    </Link>
  );
}

/** A rack ear, with the screw that holds it in. */
function Ear() {
  return (
    <span aria-hidden className="flex h-full w-2 shrink-0 flex-col items-center justify-center gap-4">
      <span className="size-1.5 rounded-full bg-linear-to-br from-[#8e8e93] to-[#48484a] shadow-[inset_0_0_0_0.5px_rgb(0_0_0/0.5)]" />
    </span>
  );
}
