import { type NostrEvent, type NostrMetadata, NSchema as n } from '@nostrify/nostrify';
import { useNostr } from '@nostrify/react';
import { useQuery } from '@tanstack/react-query';

import { useDebounce } from '@/hooks/useDebounce';

export interface ProfileSearchResult {
  pubkey: string;
  metadata: NostrMetadata;
  event: NostrEvent;
}

/** Below this a search matches most of the network, so it isn't worth sending. */
const MIN_QUERY_LENGTH = 2;

/**
 * Look up profiles by name using NIP-50.
 *
 * The two extension tokens are what make this usable as a typeahead:
 * `autocomplete:true` asks the relay to prefix-match the short name-shaped
 * fields of kind 0 rather than run a full-text search over the whole content,
 * and `sort:top` asks for the results ranked. Relays that implement neither
 * still answer the plain search, so the dropdown degrades rather than breaks.
 */
export function useSearchProfiles(query: string) {
  const { nostr } = useNostr();
  const search = useDebounce(query.trim(), 300);

  return useQuery<ProfileSearchResult[]>({
    queryKey: ['nostr', 'search-profiles', search],
    queryFn: async ({ signal }) => {
      const events = await nostr.query(
        [{ kinds: [0], search: `${search} autocomplete:true sort:top`, limit: 10 }],
        { signal: AbortSignal.any([signal, AbortSignal.timeout(5000)]) },
      );

      // Every relay answers with its own copy of a profile; keep the newest.
      const newest = new Map<string, NostrEvent>();
      for (const event of events) {
        const seen = newest.get(event.pubkey);
        if (!seen || event.created_at > seen.created_at) {
          newest.set(event.pubkey, event);
        }
      }

      const results: ProfileSearchResult[] = [];
      for (const event of newest.values()) {
        const metadata = n.json().pipe(n.metadata()).safeParse(event.content);
        if (metadata.success) {
          results.push({ pubkey: event.pubkey, metadata: metadata.data, event });
        }
      }

      return results;
    },
    enabled: search.length >= MIN_QUERY_LENGTH,
    staleTime: 30 * 1000,
    // Hold the previous names on screen so the list doesn't blink between keystrokes.
    placeholderData: (previous) => previous,
  });
}
