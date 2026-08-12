import { EventEmitter } from "node:events";
import { isIP } from "node:net";
import type { Kysely } from "kysely";
import { z } from "zod";
import type { Database } from "./db/schema.ts";
import { isHostname, normalizeHostname } from "./hostname.ts";
import type { Logger } from "./log.ts";

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
}

export type SettingKey = keyof SettingsValues;

const relayUrl = z
  .string()
  .trim()
  .transform((value, ctx) => {
    let url: URL;
    try {
      url = new URL(value);
    } catch {
      ctx.addIssue({ code: "custom", message: `Invalid relay URL: ${value}` });
      return z.NEVER;
    }
    if (url.protocol !== "wss:" && url.protocol !== "ws:") {
      ctx.addIssue({ code: "custom", message: `Relay URL must be ws:// or wss://: ${value}` });
      return z.NEVER;
    }
    return url.toString().replace(/\/$/, "");
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
      ctx.addIssue({ code: "custom", message: `Not an IP address or hostname: ${value}` });
      return z.NEVER;
    }
    return hostname;
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
  resync_interval: z.number().int().min(60).max(86400),
  // A CNAME may not coexist with any other record for the same name, so a
  // hostname target has to stand alone — advertising it beside an A record
  // would tell staff to build a zone their provider will reject.
  dns_targets: z
    .array(dnsTarget)
    .max(8)
    .refine((targets) => targets.length < 2 || targets.every((target) => isIP(target)), {
      message: "A hostname target becomes a CNAME, which must be the only record. Use IP addresses to list several.",
    }),
  allowed_host_suffixes: z.array(hostSuffix),
  tls_check_limit_per_host: z.number().int().min(1).max(10_000),
  tls_check_limit_total: z.number().int().min(1).max(100_000),
} satisfies { [K in SettingKey]: z.ZodType<SettingsValues[K]> };

export const DEFAULT_SETTINGS: SettingsValues = {
  relays: ["wss://relay.ditto.pub", "wss://relay.dreamith.to", "wss://relay.primal.net"],
  blossom_fallback_servers: [
    "https://blossom.ditto.pub/",
    "https://blossom.dreamith.to/",
    "https://blossom.primal.net/",
  ],
  max_file_size: 100 * 1024 * 1024,
  resync_interval: 600,
  dns_targets: [],
  allowed_host_suffixes: [],
  tls_check_limit_per_host: 60,
  tls_check_limit_total: 600,
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
