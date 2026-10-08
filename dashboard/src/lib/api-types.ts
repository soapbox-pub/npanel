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

/** A version as the list of versions shows it. */
export interface ListedVersion extends HostVersion {
  /** Bytes of its files stored so far, each counted once. */
  size: number;
}

/** What one site keeps on disk. */
export interface SiteStorage {
  versions: number;
  blobs: number;
  /** Every file its versions and builds keep, compressed copies included, each counted once. */
  bytes: number;
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
   * Whether, while counting, visits are reported by a tracking script added
   * to every page, rather than counted by the server as pages load. Missing
   * from servers before it was added, which only ever used the script.
   */
  analytics_script?: boolean;
  /**
   * Whether crawlers asking for a page whose path names a Nostr event — a
   * profile, a note, an article — get that event's preview and content.
   */
  link_previews: boolean;
  /**
   * Whether its pages report the errors they throw, shown on the dashboard.
   * Missing from servers before it was added.
   */
  errors?: boolean;
  /**
   * The site's Tor address, or null when Tor is off or hasn't published it yet.
   * Assigned by npanel, not by staff, and stable once it exists.
   */
  onion: string | null;
  /**
   * The certificates for its name and its redirected `www`, when npanel gets
   * certificates itself (`https_port`). Null behind a proxy, and missing
   * from servers before it was added.
   */
  certificates?: CertificateStatus[] | null;
  pinned_event_id: string | null;
  created_by: string;
  created_at: number;
  updated_at: number;
  /** The version being served right now. */
  active: HostVersion | null;
  /** The newest known version, which may still be deploying. */
  latest: HostVersion | null;
  /** How the site presents itself, read from its own build. */
  preview: SitePreview;
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
  analytics_script?: boolean;
  link_previews?: boolean;
  errors?: boolean;
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

/** One variable a site's pages read as `window.ENV`. */
export interface EnvVariable {
  name: string;
  value: string;
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
  max_site_size: number;
  max_site_files: number;
  deploy_attempts: number;
  resync_interval: number;
  /** Ready versions kept per site by the daily prune, or 0 for every one. */
  keep_versions: number;
  dns_targets: string[];
  allowed_host_suffixes: string[];
  tls_check_limit_per_host: number;
  tls_check_limit_total: number;
  /** The ACME directory certificates come from, when npanel gets them itself. */
  acme_directory: string;
  /** Who the certificate authority writes to, or empty for nobody. */
  acme_email: string;
  contact_npub: string;
  public_site_list: boolean;
  tor_enabled: boolean;
  /** The IANA time zone every site's analytics are reported in. */
  analytics_timezone: string;
  /** Which User-Agents get link previews, as a case-insensitive pattern. */
  link_preview_crawler_pattern: string;
  /** Seconds a fetched preview is served before it is fetched again. */
  link_preview_ttl: number;
  /** Lookups that may run at once; past it, crawlers get the page as published. */
  link_preview_concurrency: number;
  /** Lookups that may go to relays in a minute. Cached previews don't count. */
  link_preview_misses_per_minute: number;
  /** Previews kept on disk. */
  link_preview_max_entries: number;
  /** Days an error report is kept. Missing from servers before it was added. */
  errors_retention_days?: number;
}

/** Whether npanel is on the Tor network, and what it has published there. */
export interface TorStatus {
  enabled: boolean;
  connected: boolean;
  /** How far through joining the network npanel is, 0–100. */
  progress: number;
  services: number;
  /** Why Tor isn't working, as a sentence, or empty when nothing is wrong. */
  error: string;
}

/** One name's certificate, when npanel gets certificates itself. */
export interface CertificateStatus {
  hostname: string;
  /** When the certificate being served expires, or null when there is none. */
  expires_at: number | null;
  /** Why the last attempt to get one failed, or null if it didn't. */
  error: string | null;
  /** Attempts that have failed in a row. */
  failures: number;
  attempted_at: number | null;
}

/** How npanel's own certificates stand, when it gets them itself. */
export interface TlsStatus {
  directory: string;
  /** Names that may have a certificate. */
  names: number;
  /** Names with one that hasn't expired. */
  issued: number;
  /** Names whose last attempt failed, and why. */
  failing: CertificateStatus[];
}

/** A site as it appears to someone who can't manage it. */
export interface PublicSite {
  hostname: string;
}

/** What a site's own build says about it: its landing page's metadata, and its icon. */
export interface SitePreview {
  title: string | null;
  description: string | null;
  /** The share image, as an absolute https URL, or null. */
  image: string | null;
  /** The site's icon, as an absolute https URL, or null. */
  icon: string | null;
}

/** A public site with the preview its own landing page asks readers to show. */
export interface SiteCard extends PublicSite, SitePreview {}

/** What the logged-out homepage knows about the server it's running on. */
export interface Instance {
  /** Whoever runs this instance, or null when no admin has said. */
  contact_npub: string | null;
  dashboard_host: string;
  /** The server's version. Missing from servers before 0.2.0. */
  version?: string;
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
  /** Disk the stored files take up, compressed copies included. */
  blob_bytes: number;
  relays: string[];
  tor: TorStatus;
  /** Null behind a proxy, and missing from servers before it was added. */
  tls?: TlsStatus | null;
}

/** One site's last 24 hours, as the list of sites draws it. */
export interface Sparkline {
  visitors: number;
  /** Percent change against the 24 hours before; 100 from nothing, as Plausible has it. */
  change: number | null;
  /** Visitors in each hour, oldest first. */
  hours: number[];
}

/** How the daily prune last went. */
export interface PruneReport {
  finished_at: number;
  versions: number;
  blobs: number;
  bytes: number;
}

/** What pruning would free, and how it last went. */
export interface Storage {
  /** Stored files nothing references, which the next prune deletes. */
  unreferenced: { blobs: number; bytes: number };
  /** Null until the server has pruned since it started. */
  last_prune: PruneReport | null;
}

/** What the link preview cache holds, and how lookups have gone since the server started. */
export interface LinkPreviewStatus {
  /** Pages with a preview. */
  found: number;
  /** Pages a site's script had nothing for, remembered for a few minutes. */
  empty: number;
  /** What the cache takes up on disk. */
  bytes: number;
  /** Previews held in memory. */
  in_memory: number;
  /** Lookups running now. */
  running: number;
  stats: {
    /** Answered from memory. */
    hits: number;
    /** Handed to the preview thread: the cache on disk, then the site's script. */
    lookups: number;
    /** Lookups that ran a site's script. */
    runs: number;
    /** Runs whose script threw, or ran out of time. */
    failures: number;
    /** Pages sent without a preview because as many lookups were running as may. */
    busy: number;
    /** Pages sent without a preview because the lookup took too long. */
    timeouts: number;
    /** Lookups that didn't run a script because the minute's runs were spent. */
    over_budget: number;
  };
}

/** A preview as crawlers will be shown it, less the article in the page's body. */
export interface LinkPreview {
  title: string | null;
  description: string | null;
  image: string | null;
  /** Its `og:type`. */
  type: string | null;
  video: string | null;
  audio: string | null;
  published: number | null;
  /** Whether it has an article for crawlers to read. */
  article: boolean;
}

/** What running one page's preview script again made. */
export interface RefreshedLinkPreview {
  url: string;
  /** Null when the script had nothing for the page. */
  preview: LinkPreview | null;
}

export type IssueStatus = 'unresolved' | 'resolved' | 'ignored';

/** Errors grouped by what threw them, and where. */
export interface Issue {
  id: number;
  status: IssueStatus;
  /** `Type: message`, one line. */
  title: string;
  /** Where it happened: the innermost of the site's own frames, or the page. */
  culprit: string;
  level: 'fatal' | 'error' | 'warning' | 'info' | 'debug';
  first_seen: number;
  last_seen: number;
  events: number;
  /** Visitors it reached, each counted once a day. */
  visitors: number;
  /** The versions it was first and last seen in, by event id. */
  first_release: string | null;
  last_release: string | null;
}

export interface ListedIssue extends Issue {
  /** Reports in each of the last 24 hours, oldest first. */
  hours: number[];
}

export interface IssueDetail extends Issue {
  /** Reports in each hour of the last two weeks, oldest first. */
  hours: number[];
  /** The newest kept event's id. */
  latest_event: number | null;
}

export interface IssuePage {
  issues: ListedIssue[];
  counts: Record<IssueStatus, number>;
  /** Where the next page starts, or null on the last. */
  next: string | null;
}

export interface IssueEventSummary {
  id: number;
  event_id: string;
  received_at: number;
  release: string | null;
  url: string | null;
  browser: string;
  os: string;
}

export interface StackFrame {
  abs_path: string;
  function?: string;
  lineno?: number;
  colno?: number;
  /** Whether it is the site's own code. */
  in_app: boolean;
  /** Where it is in the original source, through the site's source map. */
  mapped?: { source: string; function?: string; lineno: number; colno: number };
  context?: { pre: string[]; line: string; post: string[] };
}

export interface ReportedException {
  type: string;
  value: string;
  mechanism?: string;
  handled?: boolean;
  /** Oldest call first. */
  frames: StackFrame[];
}

export interface Breadcrumb {
  timestamp?: number;
  category?: string;
  level?: string;
  message?: string;
  data?: Record<string, string | number | boolean>;
}

/** One report, as npanel kept it. */
export interface ErrorReport {
  event_id: string;
  level: string;
  message?: string;
  /** Innermost cause first. */
  exceptions: ReportedException[];
  breadcrumbs: Breadcrumb[];
  url?: string;
  release?: string;
  environment?: string;
  tags: Record<string, string>;
  sdk?: string;
  browser: string;
  browser_version: string;
  os: string;
  os_version: string;
  device: string;
}

export interface IssueEvent extends IssueEventSummary {
  report: ErrorReport;
}

/** One site's errors, as the list of sites shows them. */
export interface ErrorSummary {
  unresolved: number;
  events_24h: number;
}
