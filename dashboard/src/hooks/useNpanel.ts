import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import { useApi } from '@/hooks/useApi';
import { useCurrentUser } from '@/hooks/useCurrentUser';
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

export function useStatus() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('status'),
    queryFn: () => api!.status(),
    enabled: !!api,
  });
}

export function useHosts() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('hosts'),
    queryFn: async () => (await api!.hosts()).hosts,
    enabled: !!api,
    refetchInterval: LIVE_INTERVAL,
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
    mutationFn: (input: { hostname: string; address: string }) => api!.createHost(input),
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
