import type { Generated, Insertable, Selectable, Updateable } from "kysely";

/** Role of a dashboard user. Admins have unrestricted access. */
export type UserRole = "admin" | "staff";

/**
 * Lifecycle of a single site version.
 *
 * A host only serves versions that reached `ready`, so a deploy whose blobs are
 * still downloading (or which failed entirely) never replaces a working site.
 */
export type DeployStatus = "pending" | "fetching_blobs" | "ready" | "failed";

/**
 * Whether a bunker can reach the signer that actually holds its key.
 *
 * `pending` is a bunker nobody has paired yet, `error` one whose upstream
 * stopped answering. Neither is fatal to the connections minted from it: they
 * keep their keys, and re-pairing puts them back to work.
 */
export type BunkerStatus = "pending" | "connected" | "error";

export interface Database {
  users: UsersTable;
  hosts: HostsTable;
  host_grants: HostGrantsTable;
  site_events: SiteEventsTable;
  site_files: SiteFilesTable;
  blobs: BlobsTable;
  settings: SettingsTable;
  audit_log: AuditLogTable;
  deploy_log: DeployLogTable;
  bunkers: BunkersTable;
  bunker_grants: BunkerGrantsTable;
  bunker_connections: BunkerConnectionsTable;
}

export interface UsersTable {
  /** Hex pubkey. */
  pubkey: string;
  role: UserRole;
  /** Pubkey of the user who added this one, or null for the bootstrap admin. */
  added_by: string | null;
  created_at: number;
}

export interface HostsTable {
  id: Generated<number>;
  /** Lowercased hostname, eg `docs.example.com`. */
  hostname: string;
  /** Hex pubkey of the nsite author. */
  pubkey: string;
  /** 15128 (root site) or 35128 (named site). */
  kind: number;
  /** `d` tag for named sites, empty string for root sites. */
  identifier: string;
  /** Event id to serve, or null to always serve the newest ready version. */
  pinned_event_id: string | null;
  enabled: number;
  /** 0/1. Kept out of the public list, and out of sight of staff without a grant. */
  hidden: Generated<number>;
  /** 0/1. Send `www.<hostname>` here instead, when `hostname` is a root domain. */
  redirect_www: Generated<number>;
  /** 0/1. Serve the site's shell with a 200 for paths it has no file for. */
  spa: Generated<number>;
  created_by: string;
  created_at: number;
  updated_at: number;
}

export interface HostGrantsTable {
  user_pubkey: string;
  host_id: number;
  created_at: number;
}

export interface SiteEventsTable {
  /** Nostr event id. */
  event_id: string;
  pubkey: string;
  kind: number;
  identifier: string;
  /** `created_at` of the event itself, used to order versions. */
  created_at: number;
  /** Raw signed event JSON, kept verbatim so it can be re-verified or re-exported. */
  event: string;
  status: DeployStatus;
  files_total: number;
  files_ready: number;
  /** Human-readable reason the deploy failed, shown in the dashboard. */
  error: string | null;
  fetched_at: number;
  ready_at: number | null;
}

export interface SiteFilesTable {
  event_id: string;
  /** Absolute path within the site, eg `/index.html`. */
  path: string;
  sha256: string;
}

export interface BlobsTable {
  sha256: string;
  size: number;
  seen_at: number;
}

export interface SettingsTable {
  key: string;
  /** JSON-encoded value. */
  value: string;
  updated_at: number;
}

export interface AuditLogTable {
  id: Generated<number>;
  actor_pubkey: string;
  action: string;
  target: string | null;
  old_value: string | null;
  new_value: string | null;
  created_at: number;
}

export interface DeployLogTable {
  id: Generated<number>;
  event_id: string;
  level: "debug" | "info" | "warn" | "error";
  msg: string;
  /** JSON object of flat scalar fields. */
  fields: string | null;
  created_at: number;
}

/**
 * An identity npanel can sign for, backed by a signer it does not control.
 *
 * The key itself never arrives. npanel holds a NIP-46 client credential to an
 * upstream signer — a phone running Amber, usually — and forwards requests to
 * it, which is why the only secret here is `upstream_secret`, and why replacing
 * it re-points the bunker without disturbing anything minted from it.
 */
export interface BunkersTable {
  id: Generated<number>;
  /** Lowercase URL key, eg `press-team`. Stable across renames. */
  slug: string;
  name: string;
  /** Hex pubkey the upstream signs as, learned during pairing. */
  user_pubkey: string | null;
  /** The upstream's `remote-signer-pubkey`, which npanel addresses requests to. */
  upstream_pubkey: string | null;
  /** Encrypted secret key of npanel's client keypair for the upstream. */
  upstream_secret: string | null;
  /** JSON array of relay URLs the upstream is reachable on. */
  upstream_relays: string;
  /** Which `SECRET_KEY` sealed `upstream_secret`. Null before pairing. */
  key_fingerprint: string | null;
  status: BunkerStatus;
  /** Why the upstream last failed, in a sentence staff can act on. */
  error: string | null;
  last_ok_at: number | null;
  enabled: Generated<number>;
  created_by: string;
  created_at: number;
  updated_at: number;
}

export interface BunkerGrantsTable {
  user_pubkey: string;
  bunker_id: number;
  created_at: number;
}

/**
 * One client's authorization to sign through a bunker.
 *
 * Each connection answers on a keypair of its own rather than on the bunker's,
 * so revoking one is a row delete instead of a key rotation that would break
 * every other client, and two clients can't recognise each other by the pubkey
 * they talk to.
 */
export interface BunkerConnectionsTable {
  id: Generated<number>;
  bunker_id: number;
  /** Named by whoever minted it — the only handle staff will recognise. */
  name: string;
  /** Encrypted secret key of the keypair this connection answers on. */
  signer_secret: string;
  /** Pubkey clients `p`-tag. Unique across bunkers, so one filter can route. */
  signer_pubkey: string;
  /** Bound by `connect`; null until a client pairs. */
  client_pubkey: string | null;
  /** Single-use claim token from a `bunker://` URI, cleared once spent. */
  secret: string | null;
  /** JSON array of relay URLs this connection is reachable on. */
  relays: string;
  /** JSON array of NIP-46 permission specs, eg `["sign_event:1"]`. */
  permissions: string;
  created_by: string;
  created_at: number;
  last_used_at: number | null;
  /** Why the last request was refused, so staff can see what to grant. */
  last_error: string | null;
}

export type User = Selectable<UsersTable>;
export type NewUser = Insertable<UsersTable>;
export type UserUpdate = Updateable<UsersTable>;

export type Host = Selectable<HostsTable>;
export type NewHost = Insertable<HostsTable>;
export type HostUpdate = Updateable<HostsTable>;

export type Bunker = Selectable<BunkersTable>;
export type NewBunker = Insertable<BunkersTable>;
export type BunkerUpdate = Updateable<BunkersTable>;

export type BunkerConnection = Selectable<BunkerConnectionsTable>;
export type NewBunkerConnection = Insertable<BunkerConnectionsTable>;
export type BunkerConnectionUpdate = Updateable<BunkerConnectionsTable>;

export type SiteEvent = Selectable<SiteEventsTable>;
export type SiteFile = Selectable<SiteFilesTable>;
export type AuditEntry = Selectable<AuditLogTable>;
export type DeployLogEntry = Selectable<DeployLogTable>;
