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

export type User = Selectable<UsersTable>;
export type NewUser = Insertable<UsersTable>;
export type UserUpdate = Updateable<UsersTable>;

export type Host = Selectable<HostsTable>;
export type NewHost = Insertable<HostsTable>;
export type HostUpdate = Updateable<HostsTable>;

export type SiteEvent = Selectable<SiteEventsTable>;
export type SiteFile = Selectable<SiteFilesTable>;
export type AuditEntry = Selectable<AuditLogTable>;
export type DeployLogEntry = Selectable<DeployLogTable>;
