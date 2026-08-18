import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';

import { useApi } from '@/hooks/useApi';
import { useCurrentUser } from '@/hooks/useCurrentUser';
import { fetchInstance, fetchSites } from '@/lib/api';
import type { HostPatch, Settings } from '@/lib/api-types';

/** Poll while a deploy could be in progress, so progress bars actually move. */
const LIVE_INTERVAL = 5000;

/** Poll faster while someone is standing there with a phone in their hand. */
const PAIRING_INTERVAL = 1500;

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

export function useHost(hostname: string | undefined) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('host', hostname ?? ''),
    queryFn: async () => (await api!.host(hostname!)).host,
    enabled: !!api && !!hostname,
    refetchInterval: LIVE_INTERVAL,
  });
}

export function useVersions(hostname: string | undefined) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('versions', hostname ?? ''),
    queryFn: () => api!.versions(hostname!),
    enabled: !!api && !!hostname,
    refetchInterval: LIVE_INTERVAL,
  });
}

export function useDeployLog(hostname: string | undefined, eventId: string | undefined) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('deploy-log', hostname ?? '', eventId ?? ''),
    queryFn: () => api!.deployLog(hostname!, eventId!),
    enabled: !!api && !!hostname && !!eventId,
  });
}

export function useFiles(hostname: string | undefined, eventId?: string) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('files', hostname ?? '', eventId ?? 'active'),
    queryFn: () => api!.files(hostname!, eventId),
    enabled: !!api && !!hostname,
  });
}

export function useGrants(hostname: string | undefined, enabled = true) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('grants', hostname ?? ''),
    queryFn: async () => (await api!.grants(hostname!)).grants,
    enabled: !!api && !!hostname && enabled,
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
    // `hostname` names the host being changed, and the patch may rename it, so
    // the two can't share a field.
    mutationFn: (input: { hostname: string; patch: HostPatch }) =>
      api!.updateHost(input.hostname, input.patch),
    onSuccess: invalidate,
  });
}

export function useDeleteHost() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (hostname: string) => api!.deleteHost(hostname),
    onSuccess: invalidate,
  });
}

export function useSyncHost() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (hostname: string) => api!.syncHost(hostname),
    onSuccess: invalidate,
  });
}

export function useSetGrants() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { hostname: string; pubkeys: string[] }) => api!.setGrants(input.hostname, input.pubkeys),
    onSuccess: invalidate,
  });
}

export function useBunkers() {
  const api = useApi();

  return useQuery({
    queryKey: useKey('bunkers'),
    queryFn: async () => (await api!.bunkers()).bunkers,
    enabled: !!api,
  });
}

export function useBunker(slug: string | undefined) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('bunker', slug ?? ''),
    queryFn: async () => (await api!.bunker(slug!)).bunker,
    enabled: !!api && !!slug,
  });
}

export function useConnections(slug: string | undefined) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('connections', slug ?? ''),
    queryFn: async () => (await api!.connections(slug!)).connections,
    enabled: !!api && !!slug,
    refetchInterval: LIVE_INTERVAL,
  });
}

/**
 * How a pairing attempt is going.
 *
 * Polled quickly and only while one is in flight: the answer comes from
 * someone picking up their phone, so there is nothing to wait on but time.
 */
export function useBunkerPairing(slug: string | undefined, enabled: boolean) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('pairing', slug ?? ''),
    queryFn: async () => (await api!.pairing(slug!)).pairing,
    enabled: !!api && !!slug && enabled,
    refetchInterval: PAIRING_INTERVAL,
  });
}

export function useBunkerGrants(slug: string | undefined, enabled = true) {
  const api = useApi();

  return useQuery({
    queryKey: useKey('bunker-grants', slug ?? ''),
    queryFn: async () => (await api!.bunkerGrants(slug!)).grants,
    enabled: !!api && !!slug && enabled,
  });
}

export function useCreateBunker() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { name: string }) => api!.createBunker(input),
    onSuccess: invalidate,
  });
}

export function useUpdateBunker() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { slug: string; patch: { name?: string; enabled?: boolean } }) =>
      api!.updateBunker(input.slug, input.patch),
    onSuccess: invalidate,
  });
}

export function useDeleteBunker() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (slug: string) => api!.deleteBunker(slug),
    onSuccess: invalidate,
  });
}

export function useStartPairing() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { slug: string; uri?: string }) => api!.startPairing(input.slug, input.uri),
    onSuccess: invalidate,
  });
}

export function useCancelPairing() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (slug: string) => api!.cancelPairing(slug),
    onSuccess: invalidate,
  });
}

export function useTestBunker() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (slug: string) => api!.testBunker(slug),
    onSuccess: invalidate,
  });
}

export function useMintConnection() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { slug: string; name: string; permissions: string[]; uri?: string }) =>
      api!.mintConnection(input.slug, { name: input.name, permissions: input.permissions, uri: input.uri }),
    onSuccess: invalidate,
  });
}

export function useUpdateConnection() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: {
      slug: string;
      signerPubkey: string;
      patch: { name?: string; permissions?: string[] };
    }) => api!.updateConnection(input.slug, input.signerPubkey, input.patch),
    onSuccess: invalidate,
  });
}

export function useRevokeConnection() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { slug: string; signerPubkey: string }) =>
      api!.revokeConnection(input.slug, input.signerPubkey),
    onSuccess: invalidate,
  });
}

export function useSetBunkerGrants() {
  const api = useApi();
  const invalidate = useInvalidate();

  return useMutation({
    mutationFn: (input: { slug: string; pubkeys: string[] }) => api!.setBunkerGrants(input.slug, input.pubkeys),
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
