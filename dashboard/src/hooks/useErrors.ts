import { keepPreviousData, useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import { useApi } from '@/hooks/useApi';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { useServer } from '@/hooks/useServer';
import type { IssueStatus } from '@/lib/api-types';

/** How often an open list of issues refreshes, so new ones turn up without a reload. */
const REFRESH_INTERVAL = 30_000;

function useKey(...parts: unknown[]) {
  const { user } = useCurrentUser();
  const { origin } = useServer();
  return ['npanel', origin, user?.pubkey ?? 'anonymous', 'errors', ...parts];
}

/** Every site's unresolved issues and last day's reports, for the list of sites. */
export function useErrorsSummary() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('summary'),
    queryFn: () => api!.errorsSummary(),
    enabled: !!api,
    refetchInterval: 60_000,
    retry: false,
  });
}

/** A site's issues in one state, a page at a time. */
export function useIssues(hostname: string, status: IssueStatus) {
  const api = useApi();

  return useInfiniteQuery({
    queryKey: useKey(hostname, 'issues', status),
    queryFn: ({ pageParam }) => api!.issues(hostname, status, pageParam),
    initialPageParam: undefined as string | undefined,
    getNextPageParam: (last) => last.next ?? undefined,
    enabled: !!api,
    placeholderData: keepPreviousData,
    refetchInterval: REFRESH_INTERVAL,
    retry: false,
  });
}

export function useIssue(hostname: string, id: number) {
  const api = useApi();

  return useQuery({
    queryKey: useKey(hostname, 'issue', id),
    queryFn: async () => (await api!.issue(hostname, id)).issue,
    enabled: !!api && Number.isFinite(id),
    retry: false,
  });
}

/** One of an issue's events, whole; the newest when `event` is null. */
export function useIssueEvent(hostname: string, id: number, event: number | null) {
  const api = useApi();

  return useQuery({
    queryKey: useKey(hostname, 'issue', id, 'event', event),
    queryFn: () => api!.issueEvent(hostname, id, event!),
    enabled: !!api && event !== null,
    placeholderData: keepPreviousData,
    retry: false,
  });
}

export function useSetIssueStatus(hostname: string) {
  const api = useApi();
  const queryClient = useQueryClient();
  const key = useKey();

  return useMutation({
    mutationFn: (input: { id: number; status: IssueStatus }) => api!.setIssueStatus(hostname, input.id, input.status),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: key }),
  });
}
