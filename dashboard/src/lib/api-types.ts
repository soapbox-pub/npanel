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
  /** The API's identifier for a host: unique, and what every route keys on. */
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
  /** Whether `www.<hostname>` redirects here. Only acts on a root domain. */
  redirect_www: boolean;
  /** Whether unknown document paths serve the site's shell instead of a 404. */
  spa: boolean;
  /** Whether this site is served under a Content-Security-Policy. On unless turned off. */
  csp: boolean;
  /** The policy it is actually served under, or null when the flag is off. */
  csp_policy: string | null;
  /**
   * Who wrote that policy: the site, in a `<meta>` tag of its own
   * index.html, or the gateway, standing in for a site that wrote none.
   */
  csp_source: 'site' | 'gateway' | 'off';
  /**
   * Whether npanel generates a sitemap.xml and robots.txt where the site
   * publishes neither, and marks the canonical URL of every page.
   */
  seo: boolean;
  /**
   * Whether a path this deploy doesn't have falls back to the newest earlier
   * deploy that did.
   */
  merge_deployments: boolean;
  /** Whether visits to the site are counted, and shown on the dashboard. */
  analytics: boolean;
  /**
   * The site's Tor address, or null when Tor is off or hasn't published it yet.
   * Assigned by npanel, not by staff, and stable once it exists.
   */
  onion: string | null;
  pinned_event_id: string | null;
  created_by: string;
  created_at: number;
  updated_at: number;
  /** The version being served right now. */
  active: HostVersion | null;
  /** The newest known version, which may still be deploying. */
  latest: HostVersion | null;
}

/** Everything about a host that can be changed after it exists. */
export interface HostPatch {
  hostname?: string;
  address?: string;
  enabled?: boolean;
  hidden?: boolean;
  redirect_www?: boolean;
  spa?: boolean;
  csp?: boolean;
  seo?: boolean;
  merge_deployments?: boolean;
  analytics?: boolean;
  pinned_event_id?: string | null;
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
  /** Served from an earlier deploy, because the current one dropped the path. */
  inherited: boolean;
  /**
   * Who wrote it: the site, or npanel — outright (`generated`), or by
   * rewriting a file the site published (`modified`).
   */
  source: 'site' | 'generated' | 'modified';
}

/** One NIP-05 name a site answers for at `/.well-known/nostr.json`. */
export interface Nip05Name {
  /** The local part — the `bob` in `bob@example.com`. `_` means the domain itself. */
  name: string;
  pubkey: string;
  relays: string[];
  created_at: number;
  updated_at: number;
}

export interface NpanelUser {
  pubkey: string;
  npub: string;
  role: 'admin' | 'staff';
  added_by: string | null;
  created_at: number;
  hostnames: string[];
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
  tor_enabled: boolean;
  tor_control_url: string;
  tor_control_password: string;
  /** The IANA time zone every site's analytics are reported in. */
  analytics_timezone: string;
}

/** Whether npanel is reaching Tor, and what it has published there. */
export interface TorStatus {
  enabled: boolean;
  connected: boolean;
  services: number;
  /** Why Tor is unreachable, as a sentence, or empty when it isn't. */
  error: string;
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
  tor: TorStatus;
}

/** One site's last 24 hours, as the list of sites draws it. */
export interface Sparkline {
  visitors: number;
  /** Percent change against the 24 hours before; 100 from nothing, as Plausible has it. */
  change: number | null;
  /** Visitors in each hour, oldest first. */
  hours: number[];
}
