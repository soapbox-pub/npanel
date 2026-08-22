import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { NostrEvent } from "@nostrify/nostrify";
import type { Hono } from "hono";
import type { Kysely } from "kysely";
import { finalizeEvent, generateSecretKey, getPublicKey } from "nostr-tools";
import { createApp } from "../app.ts";
import { NIP98_KIND } from "../auth.ts";
import { BlobStore } from "../blobs.ts";
import { openDatabase } from "../db/index.ts";
import type { Database, UserRole } from "../db/schema.ts";
import { type DnsResolver, DnsTargets } from "../dns.ts";
import { Logger } from "../log.ts";
import type { NostrService } from "../nostr.ts";
import { createSiteServer } from "../serve-site.ts";
import { SettingsService } from "../settings.ts";
import { SiteCsp } from "../site-csp.ts";
import { SyncWorker } from "../sync.ts";
import { TorService } from "../tor.ts";

export const DASHBOARD_HOST = "panel.test";

/** A NostrService that never touches the network. */
export function fakeNostr(overrides: Partial<NostrService> = {}): NostrService {
  return {
    relays: [],
    req: async function* () {},
    query: async () => [],
    getAuthorRelays: async () => [],
    getAuthorServers: async () => [],
    fetchManifest: async () => undefined,
    blobSources: async (event: NostrEvent, fallbacks: string[]) => [
      ...event.tags.filter((tag) => tag[0] === "server").map((tag) => tag[1]),
      ...fallbacks,
    ],
    pruneRelays: () => {},
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
  blobs: BlobStore;
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
  // No test should sit through a retry's backoff.
  const sync = new SyncWorker({ db, blobs, nostr: opts.nostr ?? fakeNostr(), settings, log, retryBackoff: 0 });
  const dns = new DnsTargets({
    dashboardHost: DASHBOARD_HOST,
    settings,
    log,
    resolver: opts.resolver ?? fakeResolver(),
  });

  const siteCsp = new SiteCsp(db, blobs, log);
  const serveSite = createSiteServer({ db, blobs, siteCsp, log });
  // Inert unless a test turns `tor_enabled` on: with no control connection its
  // reconciles return immediately and it binds no port.
  const tor = new TorService({ db, settings, log, serveSite });

  const app = createApp({
    db,
    settings,
    sync,
    blobs,
    dns,
    log,
    tor,
    siteCsp,
    serveSite,
    dashboardHost: DASHBOARD_HOST,
    dashboardDir,
  });

  return {
    db,
    settings,
    sync,
    blobs,
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
      await sync.stop();
      await db.destroy();
      await rm(dir, { recursive: true, force: true });
    },
  };
}
