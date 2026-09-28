import { useQueryClient } from '@tanstack/react-query';
import { LoaderCircle } from 'lucide-react';
import { nip19 } from 'nostr-tools';
import { useEffect, useId, useMemo, useRef, useState } from 'react';

import { parsePubkey } from '@shared/pubkey';

import { PubkeyLabel } from '@/components/PubkeyLabel';
import { Avatar, AvatarFallback, AvatarImage } from '@/components/ui/avatar';
import { Input } from '@/components/ui/input';
import { type ProfileSearchResult, useSearchProfiles } from '@/hooks/useSearchProfiles';
import { shorten } from '@/lib/format';
import { cn } from '@/lib/utils';

interface PubkeySearchInputProps {
  value: string;
  onChange(value: string): void;
  id?: string;
  placeholder?: string;
  /** For a field with no visible label. */
  'aria-label'?: string;
}

/**
 * Pubkey field that searches profiles by name as you type.
 *
 * The value stays the raw text so an npub can still be pasted; picking someone
 * from the dropdown just fills their npub in. Either way the server re-parses
 * it, and the line underneath shows who is about to be added.
 */
export function PubkeySearchInput({
  value,
  onChange,
  id,
  placeholder,
  'aria-label': ariaLabel,
}: PubkeySearchInputProps) {
  const queryClient = useQueryClient();
  const listId = useId();
  const containerRef = useRef<HTMLDivElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const [open, setOpen] = useState(false);
  const [highlighted, setHighlighted] = useState(-1);

  const pubkey = useMemo(() => {
    try {
      return parsePubkey(value);
    } catch {
      return undefined;
    }
  }, [value]);

  // A pasted npub is already an answer, so there is nothing to search for.
  const { data, isFetching } = useSearchProfiles(pubkey ? '' : value);
  const results = data ?? [];

  // The last results linger while the next query is in flight, so an emptied
  // field has to close the list itself.
  const expanded = open && !pubkey && value.trim().length > 0 && results.length > 0;
  // Results shrink as the query narrows; never point past the end of them.
  const active = Math.min(highlighted, results.length - 1);

  useEffect(() => {
    if (!expanded) return;

    const handleMouseDown = (event: MouseEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    };

    document.addEventListener('mousedown', handleMouseDown);
    return () => document.removeEventListener('mousedown', handleMouseDown);
  }, [expanded]);

  useEffect(() => {
    if (active < 0) return;
    listRef.current?.querySelectorAll('[data-result]')[active]?.scrollIntoView({ block: 'nearest' });
  }, [active]);

  const select = (result: ProfileSearchResult) => {
    // We already have the profile the preview is about to ask for.
    queryClient.setQueryData(['nostr', 'author', result.pubkey], {
      event: result.event,
      metadata: result.metadata,
    });

    onChange(nip19.npubEncode(result.pubkey));
    setOpen(false);
    setHighlighted(-1);
  };

  const handleKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Escape') {
      setOpen(false);
      return;
    }

    if (!expanded) return;

    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault();
        setHighlighted(active < results.length - 1 ? active + 1 : 0);
        break;
      case 'ArrowUp':
        event.preventDefault();
        setHighlighted(active > 0 ? active - 1 : results.length - 1);
        break;
      case 'Enter':
        if (active >= 0) {
          event.preventDefault();
          select(results[active]);
        }
        break;
    }
  };

  return (
    <div className="space-y-2">
      <div ref={containerRef} className="relative">
        <Input
          id={id}
          aria-label={ariaLabel}
          value={value}
          onChange={(event) => {
            onChange(event.target.value);
            setOpen(true);
            setHighlighted(-1);
          }}
          onFocus={() => setOpen(true)}
          onBlur={(event) => {
            if (!containerRef.current?.contains(event.relatedTarget)) {
              setOpen(false);
            }
          }}
          onKeyDown={handleKeyDown}
          placeholder={placeholder}
          autoComplete="off"
          spellCheck={false}
          role="combobox"
          aria-expanded={expanded}
          aria-controls={expanded ? listId : undefined}
          aria-autocomplete="list"
          aria-activedescendant={active >= 0 ? `${listId}-${active}` : undefined}
        />

        {isFetching && (
          <LoaderCircle className="absolute right-3 top-1/2 size-4 -translate-y-1/2 animate-spin text-muted-foreground" />
        )}

        {expanded && (
          <div
            id={listId}
            ref={listRef}
            role="listbox"
            // Keep the focus in the input, so clicking a result doesn't close
            // the list out from under the click.
            onMouseDown={(event) => event.preventDefault()}
            className="absolute z-50 mt-1 max-h-64 w-full overflow-y-auto rounded-xl bg-popover p-1 shadow-xl ring-1 ring-black/10 backdrop-blur-xl dark:ring-white/10"
          >
            {results.map((result, index) => (
              <ProfileOption
                key={result.pubkey}
                id={`${listId}-${index}`}
                result={result}
                active={index === active}
                onSelect={select}
              />
            ))}
          </div>
        )}
      </div>

      {pubkey && <PubkeyLabel pubkey={pubkey} className="text-xs text-muted-foreground" />}
    </div>
  );
}

function ProfileOption({
  id,
  result,
  active,
  onSelect,
}: {
  id: string;
  result: ProfileSearchResult;
  active: boolean;
  onSelect(result: ProfileSearchResult): void;
}) {
  const { metadata, pubkey } = result;
  const name = metadata.name ?? metadata.display_name;
  const npub = nip19.npubEncode(pubkey);

  return (
    <button
      id={id}
      type="button"
      role="option"
      data-result
      aria-selected={active}
      onClick={() => onSelect(result)}
      className={cn(
        'group flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left',
        active ? 'bg-primary text-primary-foreground' : 'hover:bg-accent',
      )}
    >
      <Avatar className="size-7 shrink-0">
        {metadata.picture && <AvatarImage src={metadata.picture} alt="" />}
        <AvatarFallback className="text-[10px]">{(name ?? npub).slice(0, 2)}</AvatarFallback>
      </Avatar>

      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm">{name ?? shorten(npub)}</span>
        <span className="block truncate text-xs text-muted-foreground group-aria-selected:text-primary-foreground/80">
          {metadata.nip05 ?? shorten(npub)}
        </span>
      </span>
    </button>
  );
}
