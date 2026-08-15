import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import { useApi } from '@/hooks/useApi';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { fetchInstance, fetchSites } from '@/lib/api';
import type { Settings } from '@/lib/api-types';

/** Poll while a deploy could be in progress, so progress bars actually move. */
const LIVE_INTERVAL = 5000;

function useKey(...parts: (string | number)[]) {
  const { user } = useCurrentUser();
  return ['npanel', user?.pubkey ?? 'anonymous', ...parts];
}

export function useMe() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('me'),
    queryFn: () => api!.me(),
    enabled: !!api,
    retry: false,
  });
}

/**
 * Who runs this server. Unauthenticated, and so deliberately outside the
 * pubkey-namespaced key space — the homepage asks for it before anyone signs
 * in, and the answer is the same for everyone.
 */
export function useInstance() {
  return useQuery({
    queryKey: ['npanel', 'instance'],
    queryFn: () => fetchInstance(),
    staleTime: Infinity,
  });
}

export function useStatus() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('status'),
    queryFn: () => api!.status(),
    enabled: !!api,
  });
}

/** The DNS records staff must create for a hostname to reach this server. */
export function useDnsTarget() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('dns'),
    queryFn: () => api!.dns(),
    enabled: !!api,
    // The server caches its own detection for the same span; asking more often
    // than that only re-reads what it already decided.
    staleTime: 5 * 60 * 1000,
  });
}

export function useHosts() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('hosts'),
    queryFn: () => api!.hosts(),
    enabled: !!api,
    refetchInterval: LIVE_INTERVAL,
  });
}

/**
 * The sites this gateway will admit to serving. Unauthenticated, like
 * {@link useInstance}, because the homepage asks before anyone signs in.
 */
export function usePublicSites() {
  return useQuery({
    queryKey: ['npanel', 'sites'],
    queryFn: async () => (await fetchSites()).sites,
  });
}

export function useHost(id: number | undefined) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('host', id ?? 0),
    queryFn: async () => (await api!.host(id!)).host,
    enabled: !!api && id !== undefined,
    refetchInterval: LIVE_INTERVAL,
  });
}

export function useVersions(id: number | undefined) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('versions', id ?? 0),
    queryFn: () => api!.versions(id!),
    enabled: !!api && id !== undefined,
    refetchInterval: LIVE_INTERVAL,
  });
}

export function useDeployLog(id: number | undefined, eventId: string | undefined) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('deploy-log', id ?? 0, eventId ?? ''),
    queryFn: () => api!.deployLog(id!, eventId!),
    enabled: !!api && id !== undefined && !!eventId,
  });
}

export function useFiles(id: number | undefined, eventId?: string) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('files', id ?? 0, eventId ?? 'active'),
    queryFn: () => api!.files(id!, eventId),
    enabled: !!api && id !== undefined,
  });
}

export function useGrants(id: number | undefined, enabled = true) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('grants', id ?? 0),
    queryFn: async () => (await api!.grants(id!)).grants,
    enabled: !!api && id !== undefined && enabled,
  });
}

export function useUsers(enabled = true) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('users'),
    queryFn: async () => (await api!.users()).users,
    enabled: !!api && enabled,
  });
}

export function useSettings(enabled = true) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('settings'),
    queryFn: () => api!.settings(),
    enabled: !!api && enabled,
  });
}

export function useAuditLog(enabled = true) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('audit-log'),
    queryFn: async () => (await api!.auditLog()).entries,
    enabled: !!api && enabled,
  });
}

/** Invalidate every npanel query after a change. */
function useInvalidate() {
  const queryClient = useQueryClient();
  return () => queryClient.invalidateQueries({ queryKey: ['npanel'] });
}

export function useCreateHost() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { hostname: string; address: string; hidden?: boolean; redirect_www?: boolean }) =>
      api!.createHost(input),
    onSuccess: invalidate,
  });
}

export function useUpdateHost() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: {
      id: number;
      hostname?: string;
      address?: string;
      enabled?: boolean;
      hidden?: boolean;
      redirect_www?: boolean;
      pinned_event_id?: string | null;
    }) => {
      const { id, ...patch } = input;
      return api!.updateHost(id, patch);
    },
    onSuccess: invalidate,
  });
}

export function useDeleteHost() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (id: number) => api!.deleteHost(id),
    onSuccess: invalidate,
  });
}

export function useSyncHost() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (id: number) => api!.syncHost(id),
    onSuccess: invalidate,
  });
}

export function useSetGrants() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { id: number; pubkeys: string[] }) => api!.setGrants(input.id, input.pubkeys),
    onSuccess: invalidate,
  });
}

export function useCreateUser() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { pubkey: string; role: 'admin' | 'staff' }) => api!.createUser(input),
    onSuccess: invalidate,
  });
}

export function useUpdateUser() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { pubkey: string; role: 'admin' | 'staff' }) =>
      api!.updateUser(input.pubkey, input.role),
    onSuccess: invalidate,
  });
}

export function useDeleteUser() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (pubkey: string) => api!.deleteUser(pubkey),
    onSuccess: invalidate,
  });
}

export function useUpdateSettings() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (patch: Partial<Settings>) => api!.updateSettings(patch),
    onSuccess: invalidate,
  });
}
