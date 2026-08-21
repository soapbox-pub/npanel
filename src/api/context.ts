import type { Kysely } from "kysely";
import type { AuthUser } from "../auth.ts";
import type { BlobStore } from "../blobs.ts";
import type { Database, Host } from "../db/schema.ts";
import type { DnsTargets } from "../dns.ts";
import type { Logger } from "../log.ts";
import { encodeNaddr, formatNsiteAddress } from "../nsite-address.ts";
import type { SettingsService } from "../settings.ts";
import type { SitePreview, SitePreviews } from "../site-preview.ts";
import { activeEvent, newestEvent, resolveSitePath } from "../sites.ts";
import type { SyncWorker } from "../sync.ts";
import type { TorService } from "../tor.ts";

export interface ApiDeps {
  db: Kysely<Database>;
  settings: SettingsService;
  sync: SyncWorker;
  blobs: BlobStore;
  dns: DnsTargets;
  log: Logger;
  tor: TorService;
  dashboardHost: string;
}

export type ApiEnv = { Variables: { user: AuthUser } };

/** Whether a user may see and manage a host. */
export async function canAccessHost(db: Kysely<Database>, user: AuthUser, hostId: number): Promise<boolean> {
  if (user.role === "admin") return true;

  const grant = await db
    .selectFrom("host_grants")
    .select("host_id")
    .where("user_pubkey", "=", user.pubkey)
    .where("host_id", "=", hostId)
    .executeTakeFirst();

  return grant !== undefined;
}

/** A site as everyone else sees it: a name, and nothing about who runs it. */
export interface SiteView {
  hostname: string;
}

/** A site with the preview its own `index.html` asks link readers to show. */
export interface SiteCard extends SiteView, SitePreview {}

/**
 * The hosts npanel will name to someone who can't manage them — a stranger on
 * the homepage, or a staff user without a grant.
 *
 * Empty unless an admin turned `public_site_list` on, since which sites a
 * gateway serves is often the private part of a private deployment. A disabled
 * host is left out because it answers 404, and a hidden one because that is
 * what the flag is for.
 */
export async function listedHosts(db: Kysely<Database>, settings: SettingsService): Promise<Host[]> {
  if (!settings.get("public_site_list")) return [];

  return await db
    .selectFrom("hosts")
    .selectAll()
    .where("enabled", "=", 1)
    .where("hidden", "=", 0)
    .orderBy("hostname")
    .execute();
}

/** {@link listedHosts}, reduced to the names, for callers that show a list. */
export async function listedSites(db: Kysely<Database>, settings: SettingsService): Promise<SiteView[]> {
  const hosts = await listedHosts(db, settings);
  return hosts.map((host) => ({ hostname: host.hostname }));
}

/**
 * A listed host with its preview, read from the copy of `index.html` npanel
 * already has on disk.
 *
 * Nothing is fetched: the document being described is one this server serves,
 * so the metadata is right there in the blob store. A site with no ready
 * version, or one whose landing page carries no metadata, is still listed —
 * just as a bare name.
 */
export async function siteCard(db: Kysely<Database>, previews: SitePreviews, host: Host): Promise<SiteCard> {
  const empty = { hostname: host.hostname, title: null, description: null, image: null };

  const version = await activeEvent(db, host);
  if (!version) return empty;

  // With the SPA flag on, the shell the site would serve for `/` is the page
  // being described, wherever it is published.
  const index = await resolveSitePath(db, version.event_id, "/", { spa: host.spa === 1 });
  if (!index || index.status !== 200) return empty;

  return {
    hostname: host.hostname,
    ...(await previews.get(index.sha256, new URL(`https://${host.hostname}/`))),
  };
}

/** Record an administrative change. */
export async function audit(
  db: Kysely<Database>,
  actor: string,
  action: string,
  target: string | null,
  oldValue?: unknown,
  newValue?: unknown,
): Promise<void> {
  await db
    .insertInto("audit_log")
    .values({
      actor_pubkey: actor,
      action,
      target,
      old_value: oldValue === undefined ? null : JSON.stringify(oldValue),
      new_value: newValue === undefined ? null : JSON.stringify(newValue),
      created_at: Math.floor(Date.now() / 1000),
    })
    .execute();
}

export interface HostView {
  /** The API's identifier for a host: unique, and what every route keys on. */
  hostname: string;
  pubkey: string;
  kind: number;
  identifier: string;
  address: string;
  naddr: string;
  enabled: boolean;
  /** Kept out of the public list and away from staff without a grant. */
  hidden: boolean;
  /** Whether `www.<hostname>` redirects here. Only acts on a root domain. */
  redirect_www: boolean;
  /** Whether unknown document paths serve the site's shell instead of a 404. */
  spa: boolean;
  /** Whether this site is served under a `Content-Security-Policy`. On unless turned off. */
  csp: boolean;
  /**
   * The site's Tor address, or null when Tor is off or has not published it
   * yet. Assigned by npanel, not by staff, and stable once it exists.
   */
  onion: string | null;
  pinned_event_id: string | null;
  created_by: string;
  created_at: number;
  updated_at: number;
  /** The version currently being served, if any. */
  active: VersionView | null;
  /** The newest known version, which may still be deploying or failed. */
  latest: VersionView | null;
}

export interface VersionView {
  event_id: string;
  created_at: number;
  status: string;
  files_total: number;
  files_ready: number;
  error: string | null;
  fetched_at: number;
  ready_at: number | null;
}

/** Shape a host row for the API, including its deploy state. */
export async function serializeHost(db: Kysely<Database>, host: Host): Promise<HostView> {
  const address = { pubkey: host.pubkey, kind: host.kind, identifier: host.identifier };
  const [active, latest, onion] = await Promise.all([
    activeEvent(db, host),
    newestEvent(db, address),
    db.selectFrom("onion_services").select("address").where("host_id", "=", host.id).executeTakeFirst(),
  ]);

  return {
    hostname: host.hostname,
    pubkey: host.pubkey,
    kind: host.kind,
    identifier: host.identifier,
    address: formatNsiteAddress(address),
    naddr: encodeNaddr(address),
    enabled: host.enabled === 1,
    hidden: host.hidden === 1,
    redirect_www: host.redirect_www === 1,
    spa: host.spa === 1,
    csp: host.csp === 1,
    onion: onion?.address ?? null,
    pinned_event_id: host.pinned_event_id,
    created_by: host.created_by,
    created_at: host.created_at,
    updated_at: host.updated_at,
    active: active ? serializeVersion(active) : null,
    latest: latest ? serializeVersion(latest) : null,
  };
}

export function serializeVersion(row: {
  event_id: string;
  created_at: number;
  status: string;
  files_total: number;
  files_ready: number;
  error: string | null;
  fetched_at: number;
  ready_at: number | null;
}): VersionView {
  return {
    event_id: row.event_id,
    created_at: row.created_at,
    status: row.status,
    files_total: row.files_total,
    files_ready: row.files_ready,
    error: row.error,
    fetched_at: row.fetched_at,
    ready_at: row.ready_at,
  };
}
