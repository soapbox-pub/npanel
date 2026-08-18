import { METHOD_LABELS, formatPermission, parsePermission } from '@shared/bunker-permissions';

/**
 * Turning permission specs into things to tick, and back.
 *
 * The specs themselves are the server's — see `@shared/bunker-permissions` —
 * so what someone ticks here is exactly what the gateway will enforce. Only
 * the shape of the form lives in the dashboard.
 */

/** What a form knows about a connection's permissions. */
export interface PermissionDraft {
  /** Whether signing is allowed at all. */
  sign: boolean;
  /** Every kind, rather than the listed ones. */
  anyKind: boolean;
  /** Kinds to allow, when `anyKind` is off. */
  kinds: number[];
  /** The encryption methods, by spec name. */
  methods: string[];
}

export const EMPTY_DRAFT: PermissionDraft = { sign: true, anyKind: false, kinds: [1], methods: [] };

/** The encryption methods, in the order the form shows them. */
export const ENCRYPTION_METHODS = ['nip44_encrypt', 'nip44_decrypt', 'nip04_encrypt', 'nip04_decrypt'] as const;

export function methodLabel(method: string): string {
  return METHOD_LABELS[method as keyof typeof METHOD_LABELS] ?? method;
}

/** A spec as a person would read it, for a badge. */
export function permissionLabel(spec: string): string {
  try {
    const permission = parsePermission(spec);
    if (permission.method !== 'sign_event') return methodLabel(permission.method);
    return permission.kind === null ? 'Sign any event' : `Sign kind ${permission.kind}`;
  } catch {
    // A spec written by an older npanel, or edited by hand. Show it verbatim
    // rather than hiding a permission that is nonetheless in force.
    return spec;
  }
}

export function draftToSpecs(draft: PermissionDraft): string[] {
  const specs: string[] = [...draft.methods];

  if (draft.sign) {
    if (draft.anyKind) {
      specs.push('sign_event');
    } else {
      for (const kind of draft.kinds) specs.push(`sign_event:${kind}`);
    }
  }

  return specs;
}

export function specsToDraft(specs: string[]): PermissionDraft {
  const draft: PermissionDraft = { sign: false, anyKind: false, kinds: [], methods: [] };

  for (const spec of specs) {
    let permission: ReturnType<typeof parsePermission>;
    try {
      permission = parsePermission(spec);
    } catch {
      continue;
    }

    if (permission.method !== 'sign_event') {
      draft.methods.push(formatPermission(permission));
      continue;
    }

    draft.sign = true;
    if (permission.kind === null) {
      draft.anyKind = true;
    } else {
      draft.kinds.push(permission.kind);
    }
  }

  return draft;
}

/** Read the kinds someone typed, ignoring anything that isn't one. */
export function parseKinds(value: string): number[] {
  const kinds = value
    .split(/[,\s]+/)
    .filter(Boolean)
    .map(Number)
    .filter((kind) => Number.isInteger(kind) && kind >= 0 && kind <= 65535);

  return [...new Set(kinds)];
}
