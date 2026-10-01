import { keepPreviousData, useInfiniteQuery, useQuery } from '@tanstack/react-query';

import { useApi } from '@/hooks/useApi';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useServer } from '@/hooks/useServer';
import type { StatsQueryBody } from '@/lib/analytics/query';

/** How often the live view refreshes, as Plausible's does. */
const REALTIME_INTERVAL = 30_000;

/** Rows per page in a details dialog. */
export const DETAILS_PAGE = 100;

function useKey(...parts: unknown[]) {
  const { user } = useCurrentUser();
  const { origin } = useServer();
  return ['npanel', origin, user?.pubkey ?? 'anonymous', 'stats', ...parts];
}

/**
 * One dashboard query. The previous answer stays on screen while the next one
 * loads, so changing the period or a filter redraws rather than flashes.
 */
export function useStats(hostname: string, body: StatsQueryBody, options: { live?: boolean; enabled?: boolean } = {}) {
  const api = useApi();

  return useQuery({
    queryKey: useKey(hostname, body),
    queryFn: () => api!.statsQuery(hostname, body),
    enabled: !!api && options.enabled !== false,
    placeholderData: keepPreviousData,
    refetchInterval: options.live ? REALTIME_INTERVAL : false,
    retry: false,
  });
}

/** Every site's last 24 hours, for the list of sites. Refreshed as often as the list. */
export function useSparklines() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('sparklines'),
    queryFn: async () => (await api!.sparklines()).sparklines,
    enabled: !!api,
    refetchInterval: 60_000,
    retry: false,
  });
}

/** A details dialog's rows, a hundred at a time. */
export function useStatsPages(hostname: string, body: StatsQueryBody) {
  const api = useApi();

  return useInfiniteQuery({
    queryKey: useKey(hostname, 'pages', body),
    queryFn: ({ pageParam }) =>
      api!.statsQuery(hostname, { ...body, pagination: { limit: DETAILS_PAGE, offset: pageParam } }),
    initialPageParam: 0,
    getNextPageParam: (last, pages) => (last.results.length < DETAILS_PAGE ? undefined : pages.length * DETAILS_PAGE),
    enabled: !!api,
    retry: false,
  });
}

/** Visitors in the last five minutes, refreshed while the page is open. */
export function useCurrentVisitors(hostname: string) {
  const api = useApi();

  return useQuery({
    queryKey: useKey(hostname, 'current-visitors'),
    queryFn: () => api!.currentVisitors(hostname),
    enabled: !!api,
    refetchInterval: REALTIME_INTERVAL,
    retry: false,
  });
}

/** Values a filter could take, for its autocomplete. */
export function useSuggestions(hostname: string, filter: string, params: URLSearchParams, enabled: boolean) {
  const api = useApi();

  return useQuery({
    queryKey: useKey(hostname, 'suggestions', filter, params.toString()),
    queryFn: () => api!.suggestions(hostname, filter, params),
    enabled: !!api && enabled,
    placeholderData: keepPreviousData,
    staleTime: 60_000,
    retry: false,
  });
}
