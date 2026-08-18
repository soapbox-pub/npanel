/**
 * What a connection is allowed to ask npanel's bunker gateway to do.
 *
 * Grants are a static allow-list, fixed when a connection is minted and edited
 * later by anyone with access to the bunker. There is no prompt: npanel is an
 * unattended server, so there is nobody to answer one, and a request outside
 * the grant is refused immediately rather than left hanging until the client
 * times out.
 *
 * The spec format is NIP-46's own — `method` or `method:kind` — so the strings
 * a client puts in a `nostrconnect://` URI's `perms` are the strings stored
 * here. Shared with the dashboard, so the permissions someone ticks are
 * exactly the ones the server will enforce.
 */

/**
 * Methods every paired client may call.
 *
 * None of them reveal anything the client doesn't already have — the pubkey it
 * connected to sign for, a liveness check, the relays to use, and the two ends
 * of the session's own lifecycle. Making them grantable would only create
 * connections that are paired but unable to say so.
 */
export const IMPLICIT_METHODS = ["connect", "ping", "get_public_key", "switch_relays", "logout"] as const;

/** Methods that do something with the key, and so must be granted. */
export const GRANTABLE_METHODS = [
  "sign_event",
  "nip04_encrypt",
  "nip04_decrypt",
  "nip44_encrypt",
  "nip44_decrypt",
] as const;

export type ImplicitMethod = (typeof IMPLICIT_METHODS)[number];
export type GrantableMethod = (typeof GRANTABLE_METHODS)[number];

/** What each grantable method is called in the dashboard. */
export const METHOD_LABELS: Record<GrantableMethod, string> = {
  sign_event: "Sign events",
  nip04_encrypt: "Encrypt (NIP-04)",
  nip04_decrypt: "Decrypt (NIP-04)",
  nip44_encrypt: "Encrypt (NIP-44)",
  nip44_decrypt: "Decrypt (NIP-44)",
};

export interface Permission {
  method: GrantableMethod;
  /** An event kind for `sign_event`, or null meaning every kind. */
  kind: number | null;
}

export function isImplicitMethod(method: string): method is ImplicitMethod {
  return (IMPLICIT_METHODS as readonly string[]).includes(method);
}

export function isGrantableMethod(method: string): method is GrantableMethod {
  return (GRANTABLE_METHODS as readonly string[]).includes(method);
}

/** Parse one `method` or `method:kind` spec, throwing a usable message. */
export function parsePermission(spec: string): Permission {
  const value = spec.trim().toLowerCase();
  const [method, qualifier, ...rest] = value.split(":");

  if (!method || rest.length) {
    throw new Error(`Not a permission: ${spec}`);
  }

  if (!isGrantableMethod(method)) {
    if (isImplicitMethod(method)) {
      throw new Error(`Every connection may call ${method}, so it can't be granted separately.`);
    }
    throw new Error(`Unknown permission: ${spec}. Expected one of: ${GRANTABLE_METHODS.join(", ")}.`);
  }

  if (qualifier === undefined) {
    return { method, kind: null };
  }

  // A kind on anything else would be stored as a rule that can never match,
  // which reads in the dashboard as a grant that silently does nothing.
  if (method !== "sign_event") {
    throw new Error(`Only sign_event takes an event kind, so ${spec} would never match anything.`);
  }

  const kind = Number(qualifier);
  if (!Number.isInteger(kind) || kind < 0 || kind > 65535) {
    throw new Error(`${spec} is not a valid event kind.`);
  }

  return { method, kind };
}

export function formatPermission(permission: Permission): string {
  return permission.kind === null ? permission.method : `${permission.method}:${permission.kind}`;
}

/**
 * Parse a whole grant, dropping duplicates and anything a broader grant in the
 * same list already covers.
 *
 * `["sign_event", "sign_event:1"]` collapses to `["sign_event"]`: keeping both
 * would show staff a kind restriction that the blanket grant beside it makes
 * meaningless.
 */
export function parsePermissions(specs: string[]): Permission[] {
  const parsed = specs.map(parsePermission);
  const blanket = new Set(parsed.filter((p) => p.kind === null).map((p) => p.method));

  const seen = new Set<string>();
  const result: Permission[] = [];

  for (const permission of parsed) {
    if (permission.kind !== null && blanket.has(permission.method)) continue;
    const key = formatPermission(permission);
    if (seen.has(key)) continue;
    seen.add(key);
    result.push(permission);
  }

  return result.sort((a, b) => formatPermission(a).localeCompare(formatPermission(b)));
}

/** Whether a connection holding `granted` may make this call. */
export function allows(granted: Permission[], method: string, kind?: number): boolean {
  if (isImplicitMethod(method)) return true;
  if (!isGrantableMethod(method)) return false;

  return granted.some((permission) => {
    if (permission.method !== method) return false;
    if (permission.kind === null) return true;
    return method === "sign_event" && permission.kind === kind;
  });
}
