import { createHash } from "node:crypto";
import type { NostrEvent } from "@nostrify/nostrify";
import type { MiddlewareHandler } from "hono";
import { verifyEvent } from "nostr-tools";
import type { UserRole } from "./db/schema.ts";

/** NIP-98 HTTP auth event kind. */
export const NIP98_KIND = 27235;

/** How far an auth event's `created_at` may be from now, in seconds. */
export const MAX_AUTH_AGE = 60;

export interface AuthUser {
  pubkey: string;
  role: UserRole;
}

export class AuthError extends Error {
  constructor(
    message: string,
    readonly status: 401 | 403 = 401,
  ) {
    super(message);
    this.name = "AuthError";
  }
}

/**
 * Verify a NIP-98 `Authorization: Nostr <base64 event>` header.
 *
 * The signed `u` tag is compared against the dashboard's public URL rather
 * than the URL this process sees: behind a reverse proxy those differ (the
 * client signs `https://panel.example.com/api/hosts`, the proxy forwards
 * `http://127.0.0.1:3000/api/hosts`), and comparing the raw request URL would
 * reject every legitimate request.
 */
export async function verifyNip98(
  request: Request,
  opts: { dashboardHost: string; maxAge?: number; now?: number },
): Promise<NostrEvent> {
  const header = request.headers.get("authorization");
  if (!header) {
    throw new AuthError("Missing Authorization header. Sign in to continue.");
  }

  const token = header.match(/^Nostr (.+)$/)?.[1];
  if (!token) {
    throw new AuthError("Authorization header must use the Nostr scheme.");
  }

  let event: NostrEvent;
  try {
    event = JSON.parse(Buffer.from(token, "base64").toString("utf8")) as NostrEvent;
  } catch {
    throw new AuthError("Authorization token is not a base64-encoded event.");
  }

  if (event.kind !== NIP98_KIND) {
    throw new AuthError(`Auth event must be kind ${NIP98_KIND}.`);
  }
  if (!verifyEvent(event)) {
    throw new AuthError("Auth event signature is invalid.");
  }

  const now = opts.now ?? Math.floor(Date.now() / 1000);
  const maxAge = opts.maxAge ?? MAX_AUTH_AGE;
  if (Math.abs(now - event.created_at) > maxAge) {
    throw new AuthError("Auth event expired. Check your device's clock.");
  }

  const method = tag(event, "method");
  if (method?.toUpperCase() !== request.method.toUpperCase()) {
    throw new AuthError("Auth event method does not match the request.");
  }

  const signed = tag(event, "u");
  if (!signed) {
    throw new AuthError("Auth event is missing a u tag.");
  }
  assertUrlMatches(signed, request.url, opts.dashboardHost);

  const payload = tag(event, "payload");
  if (payload !== undefined) {
    const body = Buffer.from(await request.clone().arrayBuffer());
    const digest = createHash("sha256").update(body).digest("hex");
    if (digest !== payload) {
      throw new AuthError("Auth event payload hash does not match the request body.");
    }
  }

  return event;
}

/**
 * Require a signed-in user who is registered in the `users` table.
 *
 * Being able to sign a Nostr event proves identity, not authorization: npanel
 * is not permissionless, so an unknown pubkey is rejected with 403.
 */
export function nip98Auth(deps: {
  dashboardHost: string;
  findUser(pubkey: string): Promise<AuthUser | undefined>;
}): MiddlewareHandler<{ Variables: { user: AuthUser } }> {
  return async (c, next) => {
    let event: NostrEvent;
    try {
      event = await verifyNip98(c.req.raw, { dashboardHost: deps.dashboardHost });
    } catch (error) {
      const status = error instanceof AuthError ? error.status : 401;
      return c.json({ error: error instanceof Error ? error.message : "Unauthorized" }, status);
    }

    const user = await deps.findUser(event.pubkey);
    if (!user) {
      return c.json({ error: "This account has no access to npanel. Ask an admin to add you." }, 403);
    }

    c.set("user", user);
    await next();
  };
}

/** Require the authenticated user to be an admin. */
export function requireAdmin(): MiddlewareHandler<{ Variables: { user: AuthUser } }> {
  return async (c, next) => {
    if (c.get("user").role !== "admin") {
      return c.json({ error: "Admins only." }, 403);
    }
    await next();
  };
}

/**
 * Compare the signed URL with the request, tolerating the scheme and port
 * rewriting a reverse proxy performs. Host and path must match exactly.
 */
function assertUrlMatches(signed: string, requestUrl: string, dashboardHost: string): void {
  let url: URL;
  try {
    url = new URL(signed);
  } catch {
    throw new AuthError("Auth event u tag is not a valid URL.");
  }

  if (url.hostname.toLowerCase() !== dashboardHost.toLowerCase()) {
    throw new AuthError(`Auth event was signed for ${url.hostname}, not ${dashboardHost}.`);
  }

  const requested = new URL(requestUrl);
  if (url.pathname !== requested.pathname || url.search !== requested.search) {
    throw new AuthError("Auth event was signed for a different URL.");
  }
}

function tag(event: NostrEvent, name: string): string | undefined {
  return event.tags.find(([key]) => key === name)?.[1];
}
