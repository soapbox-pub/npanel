/** Shapes returned by the npanel API. */

export type DeployStatus = 'pending' | 'fetching_blobs' | 'ready' | 'failed';

export interface HostVersion {
  event_id: string;
  created_at: number;
  status: DeployStatus;
  files_total: number;
  files_ready: number;
  error: string | null;
  fetched_at: number;
  ready_at: number | null;
}

export interface Host {
  id: number;
  hostname: string;
  pubkey: string;
  kind: number;
  identifier: string;
  /** Canonical `kind:pubkey:identifier` form. */
  address: string;
  naddr: string;
  enabled: boolean;
  /** Kept off the public list, and out of sight of staff without access. */
  hidden: boolean;
  pinned_event_id: string | null;
  created_by: string;
  created_at: number;
  updated_at: number;
  /** The version being served right now. */
  active: HostVersion | null;
  /** The newest known version, which may still be deploying. */
  latest: HostVersion | null;
}

export interface DeployLogEntry {
  id: number;
  level: 'debug' | 'info' | 'warn' | 'error';
  msg: string;
  fields: Record<string, string | number | boolean | null> | null;
  created_at: number;
}

export interface SiteFile {
  path: string;
  sha256: string;
  size: number | null;
  stored: boolean;
}

export interface NpanelUser {
  pubkey: string;
  npub: string;
  role: 'admin' | 'staff';
  added_by: string | null;
  created_at: number;
  host_ids: number[];
}

export interface DnsRecord {
  type: 'A' | 'AAAA' | 'CNAME';
  value: string;
}

/** What a hostname's DNS should point at to reach this server. */
export interface DnsTarget {
  /** Empty when npanel couldn't work out its own address. */
  records: DnsRecord[];
  /** `settings` when an admin set the target, `dashboard_host` when detected. */
  source: 'settings' | 'dashboard_host';
  dashboard_host: string;
}

export interface Settings {
  relays: string[];
  blossom_fallback_servers: string[];
  max_file_size: number;
  deploy_attempts: number;
  resync_interval: number;
  dns_targets: string[];
  allowed_host_suffixes: string[];
  tls_check_limit_per_host: number;
  tls_check_limit_total: number;
  contact_npub: string;
  public_site_list: boolean;
}

/** A site as it appears to someone who can't manage it. */
export interface PublicSite {
  hostname: string;
}

/** A public site with the preview its own landing page asks readers to show. */
export interface SiteCard extends PublicSite {
  title: string | null;
  description: string | null;
  /** Absolute https URL, or null. */
  image: string | null;
}

/** What the logged-out homepage knows about the server it's running on. */
export interface Instance {
  /** Whoever runs this instance, or null when no admin has said. */
  contact_npub: string | null;
  dashboard_host: string;
}

export interface AuditEntry {
  id: number;
  actor_pubkey: string;
  action: string;
  target: string | null;
  old_value: unknown;
  new_value: unknown;
  created_at: number;
}

export interface Me {
  pubkey: string;
  npub: string;
  role: 'admin' | 'staff';
}

export interface ServerStatus {
  hosts: number;
  ready_versions: number;
  blobs: number;
  blob_bytes: number;
  relays: string[];
}
