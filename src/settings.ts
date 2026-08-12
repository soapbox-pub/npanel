import { EventEmitter } from "node:events";
import type { Kysely } from "kysely";
import { z } from "zod";
import type { Database } from "./db/schema.ts";
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
} satisfies { [K in SettingKey]: z.ZodType<SettingsValues[K]> };

export const DEFAULT_SETTINGS: SettingsValues = {
  relays: [
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://relay.nostr.band",
    "wss://purplepag.es",
    "wss://user.kindpag.es",
  ],
  blossom_fallback_servers: [],
  max_file_size: 100 * 1024 * 1024,
  resync_interval: 600,
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
