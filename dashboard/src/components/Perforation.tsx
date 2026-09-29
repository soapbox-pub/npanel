import { cn } from '@/lib/utils';

/** Rack-vent holes behind the top of every page, fading out before the content gets busy. */
export function Perforation({ className }: { className?: string }) {
  return (
    <div
      aria-hidden
      className={cn(
        'perforated pointer-events-none absolute inset-x-0 top-0 -z-10 h-80 [mask-image:linear-gradient(to_bottom,black,transparent)]',
        className,
      )}
    />
  );
}
