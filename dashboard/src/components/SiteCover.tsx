import { useState } from 'react';

import { SiteIcon } from '@/components/SiteIcon';
import type { SitePreview } from '@/lib/api-types';
import { siteTile } from '@/lib/site-tile';
import { cn } from '@/lib/utils';

/**
 * A site's share image, as a chat app shows a link: the picture the site
 * itself chose for when it's passed around. A site that chose none gets a
 * wash of its tile colour with its icon in the middle, so a row of cards
 * still lines up.
 */
export function SiteCover({
  hostname,
  preview,
  className,
}: {
  hostname: string;
  preview: Pick<SitePreview, 'image' | 'icon'>;
  className?: string;
}) {
  const [failed, setFailed] = useState<string | null>(null);
  const image = preview.image && failed !== preview.image ? preview.image : null;

  return (
    <div className={cn('relative aspect-[1.91/1] w-full overflow-hidden bg-muted', className)} aria-hidden>
      {image ? (
        <img
          src={image}
          alt=""
          loading="lazy"
          onError={() => setFailed(image)}
          className="size-full object-cover"
        />
      ) : (
        <>
          <div className={cn('absolute inset-0 bg-linear-to-br opacity-20', siteTile(hostname))} />
          <div className="absolute inset-0 flex items-center justify-center">
            <SiteIcon hostname={hostname} src={preview.icon} className="size-16 shadow-lg" />
          </div>
        </>
      )}
    </div>
  );
}
