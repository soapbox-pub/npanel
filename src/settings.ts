import { EventEmitter } from "node:events";
import { isIP } from "node:net";
import type { Kysely } from "kysely";
import { nip19 } from "nostr-tools";
import { z } from "zod";
import type { Database } from "./db/schema.ts";
import { isHostname, normalizeHostname } from "./hostname.ts";
import type { Logger } from "./log.ts";
import { parseRelayUrl } from "./relay-url.ts";

/** Runtime configuration, editable by admins from the dashboard. */
export interface SettingsValues {
  /**
   * Relays used for everything: watching for new site manifests, and looking
   * up NIP-65 relay lists (kind 10002) and Blossom server lists (kind 10063).
   */
  relays: string[];
  /**
   * Blossom servers tried when a manifest carries no `server` tags and its
   * author publishes no kind 10063 list. Most sites need none of these.
   */
  blossom_fallback_servers: string[];
  /** Largest single blob npanel will download, in bytes. */
  max_file_size: number;
  /**
   * How many times a version is deployed before npanel stops for now — 1
   * disables retrying.
   *
   * Attempts back off, and blobs already on disk are kept between them, so each
   * one resumes where the last left off rather than starting over.
   */
  deploy_attempts: number;
  /** Seconds between full manifest re-fetches from relays. */
  resync_interval: number;
  /**
   * What staff are told to point hostnames at, as IP addresses (an A or AAAA
   * record) or a single hostname (a CNAME).
   *
   * Empty means detect it, by resolving the dashboard hostname.
   */
  dns_targets: string[];
  /**
   * Domains hostnames may sit under, or empty to allow any hostname.
   *
   * Adding a host authorizes a certificate for it, and certificate authorities
   * meter issuance per registered domain, so this bounds how much of that
   * budget one person can spend. Existing hosts keep working when it changes.
   */
  allowed_host_suffixes: string[];
  /** Certificates `/api/tls-check` will authorize for one hostname per hour. */
  tls_check_limit_per_host: number;
  /** Certificates `/api/tls-check` will authorize across all hostnames per hour. */
  tls_check_limit_total: number;
  /**
   * Whoever runs this instance, shown on the logged-out homepage, or empty to
   * show nobody.
   *
   * npanel is software you host yourself, so a visitor who lands on someone
   * else's gateway needs a way to tell whose it is.
   */
  contact_npub: string;
  /**
   * Whether the sites this gateway serves are listed on the homepage, and to
   * staff who can't manage them.
   *
   * Off by default: which sites a gateway serves is often the interesting part
   * of a private deployment. A site can opt out of the list on its own with its
   * `hidden` flag.
   */
  public_site_list: boolean;
  /**
   * Whether every site also gets a Tor onion address.
   *
   * Off by default, because it needs a Tor daemon this instance can reach and
   * an upgrade should not start publishing sites somewhere their admin didn't
   * ask for. Turning it on takes effect immediately: npanel connects, mints an
   * address for each site that has none, and re-publishes the rest.
   */
  tor_enabled: boolean;
  /**
   * The Tor control endpoint, as `tcp://host:port` or `unix:/path/to/socket`.
   *
   * npanel drives Tor over this rather than writing `torrc`, so nothing needs
   * a Tor restart and no configuration file is shared between the two.
   */
  tor_control_url: string;
  /**
   * Password for the control port, when Tor is configured with a
   * `HashedControlPassword`.
   *
   * Usually empty: both the bundled container and a distribution's Tor package
   * authenticate with a cookie file instead, which npanel finds on its own.
   */
  tor_control_password: string;
  /**
   * The IANA time zone analytics are reported in, eg `Europe/Berlin`.
   *
   * Decides where one day's visitors end and the next day's begin, for every
   * site. One for the instance rather than one per site: a gateway is usually
   * run by one team, reading its sites' numbers on one clock.
   */
  analytics_timezone: string;
}

export type SettingKey = keyof SettingsValues;

const relayUrl = z
  .string()
  .trim()
  .transform((value, ctx) => {
    try {
      return parseRelayUrl(value);
    } catch (error) {
      ctx.addIssue({ code: "custom", message: error instanceof Error ? error.message : String(error) });
      return z.NEVER;
    }
  });

const blossomUrl = z
  .string()
  .trim()
  .transform((value, ctx) => {
    let url: URL;
    try {
      url = new URL(value);
    } catch {
      ctx.addIssue({ code: "custom", message: `Invalid Blossom server URL: ${value}` });
      return z.NEVER;
    }
    if (url.protocol !== "https:" && url.protocol !== "http:") {
      ctx.addIssue({ code: "custom", message: `Blossom server URL must be http(s): ${value}` });
      return z.NEVER;
    }
    // Trailing slash so `new URL(sha256, server)` keeps any path prefix.
    if (!url.pathname.endsWith("/")) url.pathname += "/";
    return url.toString();
  });

const hostSuffix = z
  .string()
  .trim()
  .transform((value, ctx) => {
    const suffix = normalizeHostname(value);
    if (!isHostname(suffix)) {
      ctx.addIssue({ code: "custom", message: `Not a valid domain: ${value}` });
      return z.NEVER;
    }
    return suffix;
  });

const dnsTarget = z
  .string()
  .trim()
  .transform((value, ctx) => {
    if (isIP(value)) return value;

    const hostname = normalizeHostname(value);
    if (!isHostname(hostname)) {
      ctx.addIssue({ code: "custom", message: `Not an IP address or domain: ${value}` });
      return z.NEVER;
    }
    return hostname;
  });

/**
 * A person, normalized to an npub.
 *
 * Anything that names one is accepted, because an admin will paste whatever
 * their client copied, but only the npub is kept: an nprofile's relay hints
 * would go unused — the dashboard reads profiles from its own relays — and
 * storing one canonical form spares every reader a second format to handle.
 */
const contactNpub = z
  .string()
  .trim()
  .transform((value, ctx) => {
    if (!value) return "";

    const identifier = value.replace(/^(?:web\+)?nostr:/, "");

    if (/^[0-9a-fA-F]{64}$/.test(identifier)) {
      return nip19.npubEncode(identifier.toLowerCase());
    }

    let decoded: nip19.DecodedResult;
    try {
      decoded = nip19.decode(identifier);
    } catch {
      ctx.addIssue({ code: "custom", message: `Not an npub: ${value}` });
      return z.NEVER;
    }

    switch (decoded.type) {
      case "npub":
        return nip19.npubEncode(decoded.data);
      case "nprofile":
        return nip19.npubEncode(decoded.data.pubkey);
      case "nsec":
        ctx.addIssue({ code: "custom", message: "That's a private key. Never paste an nsec anywhere." });
        return z.NEVER;
      default:
        ctx.addIssue({ code: "custom", message: `A ${decoded.type} does not name a person. Use an npub.` });
        return z.NEVER;
    }
  });

/**
 * Where Tor listens for control connections.
 *
 * Two spellings because the two ways of installing Tor differ: a distribution
 * package usually offers a unix socket owned by a group, while the container
 * npanel ships with opens a TCP port on loopback.
 */
const torControlUrl = z
  .string()
  .trim()
  .transform((value, ctx) => {
    if (value.startsWith("unix:")) {
      const path = value.slice("unix:".length);
      if (!path.startsWith("/")) {
        ctx.addIssue({ code: "custom", message: `A unix socket needs an absolute path: ${value}` });
        return z.NEVER;
      }
      return `unix:${path}`;
    }

    let url: URL;
    try {
      url = new URL(value);
    } catch {
      ctx.addIssue({ code: "custom", message: `Not a Tor control address: ${value}` });
      return z.NEVER;
    }
    if (url.protocol !== "tcp:") {
      ctx.addIssue({ code: "custom", message: `Tor control address must be tcp:// or unix:: ${value}` });
      return z.NEVER;
    }
    if (!url.hostname || !url.port) {
      ctx.addIssue({ code: "custom", message: `Tor control address needs a host and a port: ${value}` });
      return z.NEVER;
    }
    return `tcp://${url.hostname}:${url.port}`;
  });

/**
 * An IANA time zone, in the spelling the runtime's own time zone data uses.
 *
 * Checked against that data rather than a list of our own, because the same
 * data is what will do the arithmetic: a zone it doesn't know would pass a
 * list and fail every query.
 */
const timeZone = z
  .string()
  .trim()
  .transform((value, ctx) => {
    try {
      return new Intl.DateTimeFormat("en-US", { timeZone: value }).resolvedOptions().timeZone;
    } catch {
      ctx.addIssue({ code: "custom", message: `Not a time zone: ${value}. Use an IANA name like Europe/Berlin.` });
      return z.NEVER;
    }
  });

/** Per-key validation. Values are stored as JSON in the `settings` table. */
const SCHEMAS = {
  relays: z.array(relayUrl).min(1, "At least one relay is required."),
  blossom_fallback_servers: z.array(blossomUrl),
  max_file_size: z
    .number()
    .int()
    .positive()
    .max(8 * 1024 ** 3),
  deploy_attempts: z.number().int().min(1).max(10),
  resync_interval: z.number().int().min(60).max(86400),
  // A CNAME may not coexist with any other record for the same name, so a
  // hostname target has to stand alone — advertising it beside an A record
  // would tell staff to build a zone their provider will reject.
  dns_targets: z
    .array(dnsTarget)
    .max(8)
    .refine((targets) => targets.length < 2 || targets.every((target) => isIP(target)), {
      message: "A domain target becomes a CNAME, which must be the only record. Use IP addresses to list several.",
    }),
  allowed_host_suffixes: z.array(hostSuffix),
  tls_check_limit_per_host: z.number().int().min(1).max(10_000),
  tls_check_limit_total: z.number().int().min(1).max(100_000),
  contact_npub: contactNpub,
  public_site_list: z.boolean(),
  tor_enabled: z.boolean(),
  tor_control_url: torControlUrl,
  tor_control_password: z.string(),
  analytics_timezone: timeZone,
} satisfies { [K in SettingKey]: z.ZodType<SettingsValues[K]> };

export const DEFAULT_SETTINGS: SettingsValues = {
  relays: ["wss://relay.ditto.pub", "wss://relay.dreamith.to", "wss://relay.primal.net"],
  blossom_fallback_servers: [
    "https://blossom.ditto.pub/",
    "https://blossom.dreamith.to/",
    "https://blossom.primal.net/",
  ],
  max_file_size: 100 * 1024 * 1024,
  deploy_attempts: 3,
  resync_interval: 600,
  dns_targets: [],
  allowed_host_suffixes: [],
  tls_check_limit_per_host: 60,
  tls_check_limit_total: 600,
  contact_npub: "",
  public_site_list: false,
  tor_enabled: false,
  // Tor's own default control port. Correct both for a Tor installed beside
  // npanel and for the container in compose.yaml, which shares npanel's network
  // namespace precisely so that this address needs no changing.
  tor_control_url: "tcp://127.0.0.1:9051",
  tor_control_password: "",
  analytics_timezone: "UTC",
};

export const SETTING_KEYS = Object.keys(DEFAULT_SETTINGS) as SettingKey[];

/**
 * Reads and writes runtime settings, caching them in memory.
 *
 * Consumers subscribe with {@link SettingsService.onChange} and reconfigure
 * themselves live — the relay pool resubscribes, the resync timer re-arms —
 * so admins never have to restart the process after a settings change.
 */
export class SettingsService {
  private cache: SettingsValues;
  private emitter = new EventEmitter();

  private constructor(
    private readonly db: Kysely<Database>,
    private readonly log: Logger | undefined,
    initial: SettingsValues,
  ) {
    this.cache = initial;
  }

  /** Load settings from the database, seeding any that are missing. */
  static async load(db: Kysely<Database>, log?: Logger): Promise<SettingsService> {
    const rows = await db.selectFrom("settings").selectAll().execute();
    const stored = new Map(rows.map((row) => [row.key, row.value]));
    const values = { ...DEFAULT_SETTINGS };
    const missing: SettingKey[] = [];

    for (const key of SETTING_KEYS) {
      const raw = stored.get(key);
      if (raw === undefined) {
        missing.push(key);
        continue;
      }
      try {
        assign(values, key, parseSetting(key, JSON.parse(raw)));
      } catch (error) {
        // A corrupt or outdated row must not stop the server from booting.
        log?.warn("setting_invalid", { key, err_msg: String(error) });
      }
    }

    if (missing.length) {
      const now = Math.floor(Date.now() / 1000);
      await db
        .insertInto("settings")
        .values(
          missing.map((key) => ({
            key,
            value: JSON.stringify(values[key]),
            updated_at: now,
          })),
        )
        .execute();
      log?.info("settings_seeded", { keys: missing.join(",") });
    }

    return new SettingsService(db, log, values);
  }

  get<K extends SettingKey>(key: K): SettingsValues[K] {
    return this.cache[key];
  }

  /** A snapshot of every setting. */
  all(): SettingsValues {
    return { ...this.cache };
  }

  /**
   * Validate and persist a partial update, writing one audit entry per changed
   * key. Returns the keys that actually changed.
   */
  async update(patch: Partial<SettingsValues>, actorPubkey: string): Promise<SettingKey[]> {
    const now = Math.floor(Date.now() / 1000);
    const changed: SettingKey[] = [];

    for (const key of SETTING_KEYS) {
      if (!(key in patch)) continue;

      const value = parseSetting(key, patch[key]);
      const previous = this.cache[key];
      if (JSON.stringify(previous) === JSON.stringify(value)) continue;

      await this.db.transaction().execute(async (trx) => {
        await trx
          .insertInto("settings")
          .values({ key, value: JSON.stringify(value), updated_at: now })
          .onConflict((oc) => oc.column("key").doUpdateSet({ value: JSON.stringify(value), updated_at: now }))
          .execute();

        await trx
          .insertInto("audit_log")
          .values({
            actor_pubkey: actorPubkey,
            action: "settings.update",
            target: key,
            old_value: JSON.stringify(previous),
            new_value: JSON.stringify(value),
            created_at: now,
          })
          .execute();
      });

      this.cache = { ...this.cache, [key]: value };
      changed.push(key);
    }

    if (changed.length) {
      this.log?.info("settings_updated", { keys: changed.join(","), actor: actorPubkey });
      this.emitter.emit("change", changed);
    }

    return changed;
  }

  /**
   * Take on settings another thread has already written, telling subscribers
   * which ones changed.
   *
   * For the deploy thread, whose copy is kept current by the main thread
   * rather than written or read back itself.
   */
  adopt(values: SettingsValues): void {
    const changed = SETTING_KEYS.filter((key) => JSON.stringify(this.cache[key]) !== JSON.stringify(values[key]));
    if (!changed.length) return;

    this.cache = { ...values };
    this.emitter.emit("change", changed);
  }

  /** Subscribe to settings changes. Returns an unsubscribe function. */
  onChange(listener: (keys: SettingKey[]) => void): () => void {
    this.emitter.on("change", listener);
    return () => {
      this.emitter.off("change", listener);
    };
  }
}

/** Assign one key of a settings object without widening its value type. */
function assign<K extends SettingKey>(values: SettingsValues, key: K, value: SettingsValues[K]): void {
  values[key] = value;
}

/** Validate one setting value, throwing a readable error when it's invalid. */
export function parseSetting<K extends SettingKey>(key: K, value: unknown): SettingsValues[K] {
  const result = SCHEMAS[key].safeParse(value);
  if (!result.success) {
    const message = result.error.issues.map((issue) => issue.message).join(" ");
    throw new Error(`Invalid value for ${key}: ${message}`);
  }
  return result.data as SettingsValues[K];
}
