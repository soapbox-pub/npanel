import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { NostrEvent } from "@nostrify/nostrify";
import type { Hono } from "hono";
import type { Kysely } from "kysely";
import { finalizeEvent, generateSecretKey, getPublicKey } from "nostr-tools";
import { AnalyticsService } from "../analytics/service.ts";
import { createApp } from "../app.ts";
import { NIP98_KIND } from "../auth.ts";
import { BlobStore } from "../blobs.ts";
import type { HostBuilder } from "../builds.ts";
import { openDatabase } from "../db/index.ts";
import type { Database, UserRole } from "../db/schema.ts";
import { Deployer } from "../deploy/deployer.ts";
import { type DnsResolver, DnsTargets } from "../dns.ts";
import { Logger } from "../log.ts";
import type { NostrService } from "../nostr.ts";
import { ServeCache } from "../serve-cache.ts";
import { createSiteServer } from "../serve-site.ts";
import { SettingsService } from "../settings.ts";
import { SiteCsp } from "../site-csp.ts";
import { SitePreviews } from "../site-preview.ts";
import type { SyncWorker } from "../sync.ts";
import { TorService } from "../tor.ts";

export const DASHBOARD_HOST = "panel.test";

/** A NostrService that never touches the network. */
export function fakeNostr(overrides: Partial<NostrService> = {}): NostrService {
  return {
    relays: [],
    req: async function* () {},
    query: async () => [],
    getAuthorRelays: async () => [],
    getAuthorsRelays: async () => new Map(),
    getAuthorServers: async () => [],
    fetchManifest: async () => undefined,
    blobSources: async (event: NostrEvent, fallbacks: string[]) => [
      ...event.tags.filter((tag) => tag[0] === "server").map((tag) => tag[1]),
      ...fallbacks,
    ],
    clearCaches: () => {},
    close: async () => {},
    ...overrides,
  } as unknown as NostrService;
}

export interface TestUser {
  secretKey: Uint8Array;
  pubkey: string;
  /** Build an Authorization header for a request to the dashboard. */
  auth(method: string, path: string, body?: string): string;
}

export function testUser(): TestUser {
  const secretKey = generateSecretKey();

  return {
    secretKey,
    pubkey: getPublicKey(secretKey),
    auth(method, path, body) {
      const tags: string[][] = [
        ["method", method.toUpperCase()],
        ["u", `https://${DASHBOARD_HOST}${path}`],
      ];
      if (body !== undefined) {
        tags.push(["payload", createHash("sha256").update(body).digest("hex")]);
      }

      const event = finalizeEvent(
        { kind: NIP98_KIND, created_at: Math.floor(Date.now() / 1000), tags, content: "" },
        secretKey,
      );

      return `Nostr ${Buffer.from(JSON.stringify(event)).toString("base64")}`;
    },
  };
}

export interface Harness {
  db: Kysely<Database>;
  settings: SettingsService;
  sync: SyncWorker;
  /**
   * Makes the builds hosts serve from. A test that writes host or version
   * rows directly calls `builder.hostsChanged()` before expecting them served.
   */
  builder: HostBuilder;
  /** Not running unless a test starts it, which cleanup then stops. */
  analytics: AnalyticsService;
  blobs: BlobStore;
  /** Serving's cache. A test that writes host rows directly must clear it. */
  serveCache: ServeCache;
  app: Hono;
  dashboardDir: string;
  /** Add a user to the database so their requests are authorized. */
  addUser(user: TestUser, role: UserRole): Promise<void>;
  /** Make an API request as `user`, signing a NIP-98 token for it. */
  request(user: TestUser | undefined, method: string, path: string, body?: unknown): Promise<Response>;
  /** Make a request to one of the served nsite hostnames. */
  site(hostname: string, path: string, init?: RequestInit): Promise<Response>;
  cleanup(): Promise<void>;
}

/**
 * A ready build serving exactly these paths, written straight to the database.
 *
 * For tests of resolution and caching, which are about what serving does with
 * a build rather than about how one is made. Makes a host to own it when none
 * is given.
 */
export async function insertBuild(
  db: Kysely<Database>,
  paths: Record<string, string>,
  opts: { hostId?: number; eventId?: string } = {},
): Promise<number> {
  const hostId =
    opts.hostId ??
    (
      await db
        .insertInto("hosts")
        .values({
          hostname: `build-${Math.random().toString(36).slice(2)}.example.com`,
          pubkey: "0".repeat(64),
          kind: 15128,
          identifier: "",
          pinned_event_id: null,
          enabled: 1,
          created_by: "0".repeat(64),
          created_at: 1,
          updated_at: 1,
        })
        .returning("id")
        .executeTakeFirstOrThrow()
    ).id;

  const eventId = opts.eventId ?? "e".repeat(64);
  const { id } = await db
    .insertInto("host_builds")
    .values({
      host_id: hostId,
      kind: "clearnet",
      origin: "https://example.com",
      event_id: eventId,
      fingerprint: "",
      status: "ready",
      error: null,
      last_modified: 1,
      created_at: 1,
      built_at: 1,
    })
    .returning("id")
    .executeTakeFirstOrThrow();

  for (const [path, sha256] of Object.entries(paths)) {
    await db
      .insertInto("host_build_files")
      .values({ build_id: id, path, sha256, event_id: eventId, source_sha256: null })
      .execute();
  }

  return id;
}

/** A DNS resolver that answers nothing, so tests never touch the network. */
export function fakeResolver(overrides: Partial<DnsResolver> = {}): DnsResolver {
  return {
    resolve4: async () => [],
    resolve6: async () => [],
    ...overrides,
  };
}

/** A fully wired app backed by an in-memory database and a temp blob store. */
export async function createHarness(opts: { nostr?: NostrService; resolver?: DnsResolver } = {}): Promise<Harness> {
  const dir = await mkdtemp(join(tmpdir(), "npanel-test-"));
  const dashboardDir = join(dir, "dashboard");
  await mkdir(dashboardDir, { recursive: true });
  await writeFile(join(dashboardDir, "index.html"), "<!doctype html><title>npanel</title>");

  const db = await openDatabase(":memory:");
  const settings = await SettingsService.load(db);
  const blobs = new BlobStore(join(dir, "blobs"));
  const log = new Logger("error");
  const serveCache = new ServeCache(db);
  // In-process: the thread it runs on in production is DeployService's
  // concern, and has tests of its own.
  const deployer = new Deployer({
    db,
    blobs,
    nostr: opts.nostr ?? fakeNostr(),
    settings,
    log,
    servingChanged: () => serveCache.clear(),
    // No test should sit through a retry's backoff.
    retryBackoff: 0,
  });
  const { sync, builder } = deployer;
  const dns = new DnsTargets({
    dashboardHost: DASHBOARD_HOST,
    settings,
    log,
    resolver: opts.resolver ?? fakeResolver(),
  });

  const siteCsp = new SiteCsp(db, blobs, log);
  const previews = new SitePreviews(blobs);
  // Not started: no test here needs the thread, and an analytics service that
  // isn't running answers the event path with a 503 and ignores host changes.
  const analytics = new AnalyticsService({
    db,
    settings,
    log,
    config: { port: 0, address: "127.0.0.1", dataDir: dir, logLevel: "error", downloadGeo: false },
  });
  const serveSite = createSiteServer({ db, blobs, siteCsp, serveCache, analytics, log });
  // Inert unless a test turns `tor_enabled` on: with no control connection its
  // reconciles return immediately and it binds no port.
  const tor = new TorService({ db, settings, log, serveSite, serveCache, deployments: deployer });

  const app = createApp({
    db,
    settings,
    deployments: deployer,
    analytics,
    blobs,
    dns,
    log,
    tor,
    siteCsp,
    previews,
    serveCache,
    serveSite,
    dashboardHost: DASHBOARD_HOST,
    dashboardDir,
  });

  return {
    db,
    settings,
    sync,
    builder,
    analytics,
    blobs,
    serveCache,
    app,
    dashboardDir,

    async addUser(user, role) {
      await db
        .insertInto("users")
        .values({
          pubkey: user.pubkey,
          role,
          added_by: null,
          created_at: Math.floor(Date.now() / 1000),
        })
        .execute();
    },

    async request(user, method, path, body) {
      const payload = body === undefined ? undefined : JSON.stringify(body);
      const headers = new Headers({ host: DASHBOARD_HOST });
      if (payload !== undefined) headers.set("content-type", "application/json");
      if (user) headers.set("authorization", user.auth(method, path, payload));

      return await app.fetch(new Request(`http://127.0.0.1${path}`, { method, headers, body: payload }));
    },

    async site(hostname, path, init) {
      const headers = new Headers(init?.headers);
      headers.set("host", hostname);
      return await app.fetch(new Request(`http://127.0.0.1${path}`, { ...init, headers }));
    },

    async cleanup() {
      await deployer.stop();
      await analytics.stop();
      await db.destroy();
      await rm(dir, { recursive: true, force: true });
    },
  };
}
