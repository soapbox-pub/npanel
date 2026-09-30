import { createHash } from "node:crypto";
import { extname } from "node:path/posix";
import type { Kysely } from "kysely";
import { MAX_INJECT_BYTES, injectAnalytics } from "./analytics/inject.ts";
import { TRACKER_PATH, type TrackerSource, trackerScript, trackerSource } from "./analytics/tracker.ts";
import { recordBlob, recordVariantSizes } from "./blob-records.ts";
import type { BlobStore } from "./blobs.ts";
import type { BuildKind, Database, Host, HostBuild, SiteEvent } from "./db/schema.ts";
import { pruneDeployLog, writeDeployLog } from "./deploy-log.ts";
import { type LogFields, type Logger, errFields } from "./log.ts";
import { isCompressible } from "./mime.ts";
import type { NsiteAddress } from "./nsite-address.ts";
import {
  MAX_ROBOTS_BYTES,
  augmentRobots,
  robotsNeedsSitemap,
  robotsTxt,
  sitemapPaths,
  sitemapXml,
  versionDate,
} from "./seo.ts";
import type { RequestOrigin } from "./serve-site.ts";
import type { SettingsService } from "./settings.ts";
import { activeEvent, newestByPath, versionChain } from "./sites.ts";

/**
 * Bumped whenever what a build produces changes for the same inputs.
 *
 * It is part of every fingerprint, so a new npanel whose builds would come out
 * differently rebuilds every host at startup — the same "redeploy" any other
 * change to a build's inputs causes.
 */
export const BUILDER_VERSION = 2;

/** Wait before a second attempt at a build, in ms. It doubles after that. */
const RETRY_BACKOFF = 5_000;

/** Rows written per insert, under SQLite's bound-parameter limit. */
const INSERT_CHUNK = 200;

/** Extensions of the pages the analytics tracker is added to. */
const PAGE_EXTENSIONS = new Set([".html", ".htm"]);

const UTF8 = new TextEncoder();

/** One path of a build, as it goes into `host_build_files`. */
export interface BuildFile {
  sha256: string;
  /** The version that published it, or null for a file npanel generated. */
  event_id: string | null;
  /** The published blob it was rewritten from, if it was. */
  source_sha256: string | null;
}

/** Everything one build of a host is made from. */
interface BuildTarget {
  host: Host;
  kind: BuildKind;
  /** The name and scheme its generated files address the site by. */
  origin: RequestOrigin;
  /** The version it serves. */
  event: SiteEvent;
  /** The versions it merges, newest first, starting with {@link event}. */
  chain: string[];
  /** The tracker its pages load, when the host counts visits. */
  tracker: TrackerSource | undefined;
  fingerprint: string;
}

export interface HostBuilderDeps {
  db: Kysely<Database>;
  blobs: BlobStore;
  settings: SettingsService;
  /**
   * Told whenever what a host serves changes, so serving's cache is cleared
   * and the next request is answered from the new build. Serving may be on
   * another thread, so this is a notification rather than the cache itself.
   */
  servingChanged(): void;
  log: Logger;
  /** Overrides {@link RETRY_BACKOFF}, so tests don't have to wait one out. */
  retryBackoff?: number;
}

/**
 * The build a host is serving under one kind of name, if it has one.
 *
 * The newest ready one: a build still being made, or one that failed, never
 * displaces the build before it.
 */
export async function currentBuild(
  db: Kysely<Database>,
  hostId: number,
  kind: BuildKind,
): Promise<HostBuild | undefined> {
  return await db
    .selectFrom("host_builds")
    .selectAll()
    .where("host_id", "=", hostId)
    .where("kind", "=", kind)
    .where("status", "=", "ready")
    .orderBy("id", "desc")
    .limit(1)
    .executeTakeFirst();
}

/**
 * Makes the finished bundle each host serves, and keeps it current.
 *
 * A build is a host's version chain flattened into one path map, plus every
 * file npanel generates or rewrites for that host — a sitemap, a `robots.txt`
 * with a line appended. All of that is worked out here, once, and stored as
 * ordinary blobs with their compressed copies, so serving a request is a
 * lookup and a `sendfile` whatever the file is. Nothing a site serves is
 * computed per request except headers.
 *
 * Hosts are reconciled rather than tracked, the way Tor's onions are: every
 * trigger — a deploy finishing, a host being edited, an onion being minted,
 * startup — asks what each host's build *should* be made of, hashes that into
 * a fingerprint, and builds only where the fingerprint differs from the build
 * being served. Nothing has to be remembered between two runs, so an
 * interrupted one is repaired by the next.
 *
 * A host serves only a `ready` build, and a new build replaces the old one
 * only once it is complete — the same invariant deploys keep, one level up. A
 * host that has never been built answers 503, like a site that has never
 * deployed.
 */
export class HostBuilder {
  private readonly db: Kysely<Database>;
  private readonly blobs: BlobStore;
  private readonly settings: SettingsService;
  private readonly servingChanged: () => void;
  private readonly log: Logger;
  private readonly retryBackoff: number;

  /** Hosts waiting to be reconciled. */
  private readonly pending = new Set<number>();
  /** Whether every host is waiting, which a startup or a bulk change asks for. */
  private everything = false;
  private pumping: Promise<void> | undefined;
  private stopped = false;

  /** Attempts spent on one build, while it is being retried. */
  private readonly attempts = new Map<string, number>();
  private readonly retries = new Set<ReturnType<typeof setTimeout>>();

  constructor(deps: HostBuilderDeps) {
    this.db = deps.db;
    this.blobs = deps.blobs;
    this.settings = deps.settings;
    this.servingChanged = deps.servingChanged;
    this.log = deps.log;
    this.retryBackoff = deps.retryBackoff ?? RETRY_BACKOFF;
  }

  /**
   * Clear the debris of an interrupted run and reconcile every host.
   *
   * A build still marked `building` can only be left over from a process that
   * stopped mid-build; its files may be partial, and nothing would ever finish
   * it. The reconcile that follows makes a fresh one wherever it is needed.
   */
  async start(): Promise<void> {
    await this.db.deleteFrom("host_builds").where("status", "=", "building").execute();
    void this.hostsChanged();
  }

  /** Stop building. Resolves once any build in progress has settled. */
  async stop(): Promise<void> {
    this.stopped = true;
    for (const timer of this.retries) clearTimeout(timer);
    this.retries.clear();
    await this.whenIdle();
  }

  /**
   * Reconcile some hosts, or all of them. Safe to call at any time.
   *
   * Returns when the queue has drained, which callers answering a request do
   * not wait for: a rebuild can take longer than the edit that caused it
   * should, and the previous build serves in the meantime.
   */
  hostsChanged(hostIds?: number[]): Promise<void> {
    if (this.stopped) return Promise.resolve();

    if (hostIds) {
      for (const id of hostIds) this.pending.add(id);
    } else {
      this.everything = true;
    }

    this.pumping ??= this.pump().finally(() => {
      this.pumping = undefined;
    });

    return this.whenIdle();
  }

  /** Reconcile every host serving a site, because one of its versions changed. */
  async siteChanged(address: NsiteAddress): Promise<void> {
    const hosts = await this.db
      .selectFrom("hosts")
      .select("id")
      .where("pubkey", "=", address.pubkey)
      .where("kind", "=", address.kind)
      .where("identifier", "=", address.identifier)
      .execute();

    if (hosts.length) await this.hostsChanged(hosts.map((host) => host.id));
  }

  /** Resolves once nothing is waiting to be built. */
  async whenIdle(): Promise<void> {
    while (this.pumping) {
      await this.pumping;
    }
  }

  private async pump(): Promise<void> {
    while (!this.stopped) {
      let ids: number[];

      if (this.everything) {
        this.everything = false;
        this.pending.clear();
        ids = (await this.db.selectFrom("hosts").select("id").execute()).map((host) => host.id);
      } else if (this.pending.size) {
        ids = [...this.pending];
        this.pending.clear();
      } else {
        return;
      }

      for (const id of ids) {
        if (this.stopped) return;
        await this.reconcile(id);
      }
    }
  }

  /**
   * Make one host's builds match what it should be serving.
   *
   * Never throws: a host whose build fails is logged and retried, and must not
   * stop the hosts queued behind it.
   */
  private async reconcile(hostId: number): Promise<void> {
    let targets: BuildTarget[] | undefined;
    try {
      targets = await this.targets(hostId);
    } catch (error) {
      this.log.error("build_plan_failed", { host_id: hostId, ...errFields(error) });
      return;
    }

    if (!targets) return;

    for (const target of targets) {
      if (this.stopped) return;

      const current = await currentBuild(this.db, hostId, target.kind);
      if (current?.fingerprint === target.fingerprint) {
        this.attempts.delete(attemptKey(target));
        continue;
      }

      try {
        await this.build(target);
        this.attempts.delete(attemptKey(target));
      } catch (error) {
        await this.fail(target, error);
      }
    }
  }

  /**
   * What a host's builds should be made of, or undefined to leave it be.
   *
   * A disabled host is left with whatever builds it has: it serves nothing, and
   * re-enabling it goes straight back to a build that is still current. A host
   * with nothing to serve loses its builds, since the last one belongs to a
   * version it no longer points at — a retarget to a site that has never
   * deployed must answer 503, not go on serving the old site.
   */
  private async targets(hostId: number): Promise<BuildTarget[] | undefined> {
    const host = await this.db.selectFrom("hosts").selectAll().where("id", "=", hostId).executeTakeFirst();
    if (!host || host.enabled !== 1) return undefined;

    const event = await activeEvent(this.db, host);
    if (!event) {
      const dropped = await this.db.deleteFrom("host_builds").where("host_id", "=", hostId).executeTakeFirst();
      if (dropped.numDeletedRows > 0n) this.servingChanged();
      return undefined;
    }

    const chain = await versionChain(this.db, host, event);
    const onion = await this.db
      .selectFrom("onion_services")
      .select("address")
      .where("host_id", "=", hostId)
      .executeTakeFirst();

    const tracker = host.analytics === 1 ? await trackerSource() : undefined;

    const origins: [BuildKind, RequestOrigin][] = [["clearnet", { hostname: host.hostname, scheme: "https" }]];
    // The onion gets a build of its own because its generated files name it:
    // a sitemap fetched over Tor lists onion URLs. What the two share is
    // stored once, being content-addressed; only the rows are doubled.
    if (onion) origins.push(["onion", { hostname: onion.address, scheme: "http" }]);

    return origins.map(([kind, origin]) => ({
      host,
      kind,
      origin,
      event,
      chain,
      tracker,
      fingerprint: fingerprint(host, origin, chain, tracker),
    }));
  }

  /** Make one build and put it live. Throws if it can't, leaving the old one serving. */
  private async build(target: BuildTarget): Promise<void> {
    const { host, kind, origin, event } = target;
    const now = Math.floor(Date.now() / 1000);
    const started = Date.now();

    const { id } = await this.db
      .insertInto("host_builds")
      .values({
        host_id: host.id,
        kind,
        origin: originUrl(origin),
        event_id: event.event_id,
        fingerprint: target.fingerprint,
        status: "building",
        error: null,
        last_modified: lastModified(event),
        created_at: now,
        built_at: null,
      })
      .returning("id")
      .executeTakeFirstOrThrow();

    const fields: LogFields = { hostname: origin.hostname, kind, build_id: id };
    await writeDeployLog(this.db, event.event_id, "info", "build_started", {
      ...fields,
      versions: target.chain.length,
    });

    let files: Map<string, BuildFile>;
    try {
      files = await this.assemble(target);
    } catch (error) {
      await this.db
        .updateTable("host_builds")
        .set({ status: "failed", error: message(error) })
        .where("id", "=", id)
        .execute();
      throw error;
    }

    const previous = await currentBuild(this.db, host.id, kind);

    await this.db.transaction().execute(async (trx) => {
      const rows = [...files].map(([path, file]) => ({ build_id: id, path, ...file }));
      for (let i = 0; i < rows.length; i += INSERT_CHUNK) {
        await trx
          .insertInto("host_build_files")
          .values(rows.slice(i, i + INSERT_CHUNK))
          .execute();
      }

      await trx
        .updateTable("host_builds")
        .set({ status: "ready", built_at: Math.floor(Date.now() / 1000) })
        .where("id", "=", id)
        .execute();

      // The build being replaced is kept one generation longer: a request
      // that looked it up a moment before this commit may still be resolving
      // a path in it. Anything older has nobody left reading it.
      const keep = previous ? [id, previous.id] : [id];
      await trx
        .deleteFrom("host_builds")
        .where("host_id", "=", host.id)
        .where("kind", "=", kind)
        .where("id", "not in", keep)
        .execute();
    });

    this.servingChanged();

    this.log.info("build_ready", { ...fields, files: files.size, ms: Date.now() - started });
    await writeDeployLog(this.db, event.event_id, "info", "build_ready", { ...fields, files: files.size });
    await pruneDeployLog(this.db, event.event_id);
  }

  /** Every path the build answers, and the blob behind each. */
  private async assemble(target: BuildTarget): Promise<Map<string, BuildFile>> {
    const rows = await this.db
      .selectFrom("site_files")
      .select(["path", "sha256", "event_id"])
      .where("event_id", "in", target.chain)
      .execute();

    const files = new Map<string, BuildFile>();
    for (const [path, row] of newestByPath(rows, target.chain)) {
      files.set(path, { sha256: row.sha256, event_id: row.event_id, source_sha256: null });
    }

    if (target.host.seo !== 0) await this.seo(target, files);
    if (target.tracker) await this.analytics(target, target.tracker, files);

    return files;
  }

  /**
   * The tracker, and every page of the site loading it.
   *
   * Each page is rewritten once per distinct blob — a site whose pages share
   * one shell rewrites it once — and stored like any other file, so a page
   * that comes out the same as last build's costs a hash and nothing more.
   * The rewritten page names the published one as its source, which is how
   * the dashboard can say what npanel changed.
   */
  private async analytics(target: BuildTarget, tracker: TrackerSource, files: Map<string, BuildFile>): Promise<void> {
    const script = UTF8.encode(trackerScript(tracker, target.host.hostname));
    const integrity = `sha384-${createHash("sha384").update(script).digest("base64")}`;

    const rewritten = new Map<string, string | null>();
    let pages = 0;
    let skipped = 0;

    for (const [path, file] of files) {
      if (!PAGE_EXTENSIONS.has(extname(path).toLowerCase())) continue;

      let sha256 = rewritten.get(file.sha256);
      if (sha256 === undefined) {
        sha256 = null;
        const size = await this.blobs.size(file.sha256);
        if (size !== undefined && size <= MAX_INJECT_BYTES) {
          const page = injectAnalytics(await this.blobs.readBytes(file.sha256, MAX_INJECT_BYTES), integrity);
          if (page) sha256 = await this.store(path, page);
        }
        rewritten.set(file.sha256, sha256);
      }

      if (sha256 === null) {
        skipped++;
        continue;
      }

      files.set(path, { sha256, event_id: file.event_id, source_sha256: file.sha256 });
      pages++;
    }

    files.set(TRACKER_PATH, { sha256: await this.store(TRACKER_PATH, script), event_id: null, source_sha256: null });

    await writeDeployLog(this.db, target.event.event_id, "debug", "analytics_added", {
      hostname: target.origin.hostname,
      kind: target.kind,
      pages,
      // Too large to rewrite, or fragments with no document to add a script to.
      skipped,
    });
  }

  /**
   * The sitemap and `robots.txt` a site gets for the sake of search engines.
   *
   * Each is generated only where the site published no file at that path, the
   * same shadow-only-if-absent rule `nostr.json` follows in reverse. A
   * published `robots.txt` is the one exception: one that names no sitemap
   * gets npanel's `Sitemap:` line appended — see {@link augmentRobots}.
   *
   * The sitemap lists the served version's own pages, even on a merged host.
   * Merging keeps a withdrawn URL *answering*, for the pages and caches still
   * asking for it; inviting a crawler to index a page the latest deploy
   * deleted would be npanel contradicting the site about what it publishes.
   */
  private async seo(target: BuildTarget, files: Map<string, BuildFile>): Promise<void> {
    const { origin, event } = target;

    if (!files.has("/sitemap.xml")) {
      const xml = sitemapXml(origin, await sitemapPaths(this.db, event.event_id), versionDate(event));
      files.set("/sitemap.xml", { sha256: await this.store("/sitemap.xml", xml), event_id: null, source_sha256: null });
    }

    const robots = files.get("/robots.txt");

    if (!robots) {
      const text = robotsTxt(origin);
      files.set("/robots.txt", { sha256: await this.store("/robots.txt", text), event_id: null, source_sha256: null });
      return;
    }

    const size = await this.blobs.size(robots.sha256);
    if (size === undefined || size > MAX_ROBOTS_BYTES) return;

    const published = await this.blobs.readText(robots.sha256, MAX_ROBOTS_BYTES);
    if (!robotsNeedsSitemap(published)) return;

    files.set("/robots.txt", {
      sha256: await this.store("/robots.txt", augmentRobots(published, origin)),
      event_id: robots.event_id,
      source_sha256: robots.sha256,
    });
  }

  /**
   * Store a file npanel made, exactly as a downloaded one is stored.
   *
   * Content-addressed like everything else, so a file that comes out the same
   * as last build's is already on disk with its compressed copies, and costs
   * nothing but the hash. Compression failing is never fatal, for the same
   * reason it isn't during a deploy: an uncompressed file is a slower site,
   * not a broken one.
   */
  private async store(path: string, content: string | Uint8Array): Promise<string> {
    const bytes = typeof content === "string" ? UTF8.encode(content) : content;
    const sha256 = createHash("sha256").update(bytes).digest("hex");

    if (!(await this.blobs.has(sha256))) await this.blobs.put(sha256, bytes);
    await recordBlob(this.db, sha256, bytes.byteLength);

    if (isCompressible(path)) {
      try {
        await this.blobs.ensureVariants(sha256);
      } catch (error) {
        this.log.warn("blob_compress_failed", { sha256, ...errFields(error) });
      }
    }
    await recordVariantSizes(this.db, this.blobs, sha256);

    return sha256;
  }

  /**
   * Record a failed build, and retry it if any attempts are left.
   *
   * Like a deploy, and counted per run in the same way: a later trigger is a
   * fresh reason to believe it could work, and starts a fresh run.
   */
  private async fail(target: BuildTarget, error: unknown): Promise<void> {
    const key = attemptKey(target);
    const attempt = (this.attempts.get(key) ?? 0) + 1;
    const again = attempt < this.settings.get("deploy_attempts") && !this.stopped;
    const fields: LogFields = { hostname: target.origin.hostname, kind: target.kind, attempt };

    this.log.error("build_failed", { ...fields, ...errFields(error) });
    await writeDeployLog(this.db, target.event.event_id, "error", "build_failed", {
      ...fields,
      err_msg: message(error),
    });

    // One failure explains itself; the ones before it would only repeat it.
    const latest = await this.db
      .selectFrom("host_builds")
      .select("id")
      .where("host_id", "=", target.host.id)
      .where("kind", "=", target.kind)
      .where("status", "=", "failed")
      .orderBy("id", "desc")
      .limit(1)
      .executeTakeFirst();
    if (latest) {
      await this.db
        .deleteFrom("host_builds")
        .where("host_id", "=", target.host.id)
        .where("kind", "=", target.kind)
        .where("status", "=", "failed")
        .where("id", "<", latest.id)
        .execute();
    }

    if (!again) {
      this.attempts.delete(key);
      return;
    }

    this.attempts.set(key, attempt);
    const timer = setTimeout(
      () => {
        this.retries.delete(timer);
        void this.hostsChanged([target.host.id]);
      },
      this.retryBackoff * 2 ** (attempt - 1),
    );
    timer.unref?.();
    this.retries.add(timer);
  }
}

/**
 * A hash of everything a build's output depends on.
 *
 * The origin covers the hostname and onion address its generated files name,
 * the chain covers which bytes it serves, and the flags cover what it
 * generates. The hostname is there too because the tracker reports under it
 * from the onion as well, and the tracker's own hash because a new release of
 * it is a new file every page loads. Anything that affects only headers — the
 * CSP switch, SPA mode — is read per request and deliberately left out, so
 * flipping one rebuilds nothing.
 */
function fingerprint(host: Host, origin: RequestOrigin, chain: string[], tracker: TrackerSource | undefined): string {
  const inputs = {
    builder: BUILDER_VERSION,
    origin: originUrl(origin),
    hostname: host.hostname,
    chain,
    seo: host.seo,
    tracker: tracker?.sha256 ?? null,
  };

  return createHash("sha256").update(JSON.stringify(inputs)).digest("hex");
}

function attemptKey(target: BuildTarget): string {
  return `${target.host.id}:${target.kind}:${target.fingerprint}`;
}

function originUrl(origin: RequestOrigin): string {
  return `${origin.scheme}://${origin.hostname}`;
}

/**
 * `Last-Modified` for a build's responses, in epoch seconds.
 *
 * The served version's date, capped at the moment npanel saw it: a manifest's
 * `created_at` is whatever its author put there, including dates in the future
 * or beyond what `Date` can represent.
 */
function lastModified(event: SiteEvent): number {
  const seconds = Number.isFinite(event.created_at) ? Math.min(event.created_at, event.fetched_at) : event.fetched_at;
  return Math.max(seconds, 0);
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
