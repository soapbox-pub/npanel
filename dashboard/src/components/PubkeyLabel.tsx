import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { useAuthor } from '@/hooks/useAuthor';
import { safeNpub, shorten } from '@/lib/format';
import { cn } from '@/lib/utils';

interface PubkeyLabelProps {
  pubkey: string;
  className?: string;
  /** Hide the avatar when space is tight. */
  compact?: boolean;
}

/** A pubkey shown as its profile name when we have one, and a short npub otherwise. */
export function PubkeyLabel({ pubkey, className, compact }: PubkeyLabelProps) {
  const author = useAuthor(pubkey);
  const npub = safeNpub(pubkey);
  const name = author.data?.metadata?.name;
  const picture = author.data?.metadata?.picture;

  return (
    <span className={cn('inline-flex items-center gap-2 min-w-0', className)}>
      {!compact && (
        <Avatar className="h-5 w-5 shrink-0">
          {picture && <AvatarImage src={picture} alt="" />}
          <AvatarFallback className="text-[10px]">{(name ?? npub).slice(0, 2)}</AvatarFallback>
        </Avatar>
      )}
      <span className="truncate" title={npub}>
        {name ?? shorten(npub)}
      </span>
    </span>
  );
}
