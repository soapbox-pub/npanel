import type { NostrEvent } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import { recordBlob, recordVariantSizes } from "./blob-records.ts";
import { BlobDownloadError, type BlobStore } from "./blobs.ts";
import type { HostBuilder } from "./builds.ts";
import type { Database, DeployStatus } from "./db/schema.ts";
import { pruneDeployLog, writeDeployLog } from "./deploy-log.ts";
import { type LogFields, type Logger, errFields } from "./log.ts";
import { isSiteManifest, manifestAddress } from "./manifest.ts";
import { isCompressible } from "./mime.ts";
import { type NostrService, blobUrl } from "./nostr.ts";
import { type NsiteAddress, SITE_KINDS } from "./nsite-address.ts";
import type { SettingsService } from "./settings.ts";
import { listConfiguredSites, newestEvent, parseStoredEvent, storeManifest } from "./sites.ts";

/** Blobs downloaded in parallel within one deploy. */
const BLOB_CONCURRENCY = 4;

/** Wait before a second attempt at a deploy, in ms. It doubles after that. */
const RETRY_BACKOFF = 5_000;

export interface SyncWorkerDeps {
  db: Kysely<Database>;
  blobs: BlobStore;
  nostr: NostrService;
  settings: SettingsService;
  /** Told when a version becomes ready, so the hosts serving it are rebuilt. */
  builder: HostBuilder;
  log: Logger;
  /** Overrides {@link RETRY_BACKOFF}, so tests don't have to wait one out. */
  retryBackoff?: number;
}

/** A version waiting its turn to deploy. */
interface QueuedDeploy {
  eventId: string;
  /** Epoch ms before which it must not be attempted, for a retry's backoff. */
  after: number;
}

/**
 * Keeps local storage in sync with the nsites configured in the dashboard.
 *
 * Three things drive it:
 *
 * 1. A live subscription to the configured relays for new manifest events.
 * 2. A periodic full re-fetch, so a manifest missed on the wire (relay
 *    downtime, a restart) is still picked up.
 * 3. A blob queue that downloads every file of a new version before that
 *    version is allowed to serve — which it does once the builder has made
 *    each host serving it a build of it.
 *
 * Every step also writes to the `deploy_log` table, which the dashboard shows
 * per version — a failed deploy explains itself without touching the server.
 */
export class SyncWorker {
  private readonly db: Kysely<Database>;
  private readonly blobs: BlobStore;
  private readonly nostr: NostrService;
  private readonly settings: SettingsService;
  private readonly builder: HostBuilder;
  private readonly log: Logger;
  private readonly retryBackoff: number;

  private controller = new AbortController();
  private subscription: AbortController | undefined;
  private resyncTimer: ReturnType<typeof setInterval> | undefined;
  private unsubscribeSettings: (() => void) | undefined;

  private queue: QueuedDeploy[] = [];
  /** Attempts already spent on a version, while it is being retried. */
  private attempts = new Map<string, number>();
  /** Cuts short the wait for the next retry, when there's work to do sooner. */
  private wake: (() => void) | undefined;
  private pumping: Promise<void> | undefined;
  private resyncing: Promise<void> | undefined;
  private resyncAgain = false;
  private running = false;

  constructor(deps: SyncWorkerDeps) {
    this.db = deps.db;
    this.blobs = deps.blobs;
    this.nostr = deps.nostr;
    this.settings = deps.settings;
    this.builder = deps.builder;
    this.log = deps.log;
    this.retryBackoff = deps.retryBackoff ?? RETRY_BACKOFF;
  }

  /** Begin syncing. Resolves once the initial resync has been kicked off. */
  async start(): Promise<void> {
    if (this.running) return;
    this.running = true;

    // stop() aborts this controller, so a restarted worker needs a fresh one
    // or every fetch it makes is cancelled before it begins.
    this.controller = new AbortController();

    // Temp files can only be leftovers from a download interrupted by a crash.
    await this.blobs.clearTemp();

    this.unsubscribeSettings = this.settings.onChange((keys) => {
      if (keys.includes("relays")) {
        this.nostr.pruneRelays();
        this.nostr.clearCaches();
        void this.restartSubscription();
      }
      if (keys.includes("resync_interval")) {
        this.scheduleResync();
      }
      if (keys.includes("blossom_fallback_servers") || keys.includes("max_file_size")) {
        // New servers or a bigger limit may be exactly what a failed deploy needed.
        void this.retryIncomplete();
      }
    });

    this.scheduleResync();
    await this.restartSubscription();

    void this.resyncAll();

    this.log.info("sync_started", { relays: this.nostr.relays.length });
  }

  /** Stop syncing and release relay connections. */
  async stop(): Promise<void> {
    if (!this.running) return;
    this.running = false;

    this.unsubscribeSettings?.();
    this.unsubscribeSettings = undefined;

    if (this.resyncTimer) clearInterval(this.resyncTimer);
    this.resyncTimer = undefined;

    this.subscription?.abort();
    this.controller.abort();

    await this.pumping?.catch(() => {});
    this.attempts.clear();
    await this.nostr.close();

    this.log.info("sync_stopped");
  }

  /**
   * Called when hosts are added, retargeted, or removed. Restarts the live
   * subscription and fetches anything newly configured right away.
   */
  async hostsChanged(): Promise<void> {
    if (!this.running) return;
    await this.restartSubscription();
    void this.resyncAll();
  }

  /** Resolves once the deploy queue has drained. */
  async whenIdle(): Promise<void> {
    while (this.pumping) {
      await this.pumping;
    }
  }

  /** Fetch a site's newest manifest now, and deploy it if it isn't ready. */
  async syncSite(address: NsiteAddress): Promise<void> {
    const event = await this.nostr.fetchManifest(address, { signal: this.controller.signal });

    if (event) {
      await this.ingest(event);
    } else {
      this.log.debug("manifest_not_found", { pubkey: address.pubkey, identifier: address.identifier });
    }

    // Pick up a version that was stored earlier but never finished deploying.
    const newest = await newestEvent(this.db, address);
    if (newest && newest.status !== "ready") {
      this.enqueue(newest.event_id);
    }
  }

  /**
   * Re-fetch every configured site from relays.
   *
   * Passes never overlap — the timer firing while a host change is still being
   * fetched would duplicate every round trip — but a call made mid-pass isn't
   * dropped either: the walk may already be past whatever prompted it, so one
   * more pass is queued behind the current one.
   */
  async resyncAll(): Promise<void> {
    if (this.resyncing) {
      this.resyncAgain = true;
      await this.resyncing;
      return;
    }

    this.resyncing = this.resyncUntilSettled().finally(() => {
      this.resyncing = undefined;
    });

    await this.resyncing;
  }

  private async resyncUntilSettled(): Promise<void> {
    do {
      this.resyncAgain = false;
      await this.runResync();
    } while (this.resyncAgain && this.running);
  }

  private async runResync(): Promise<void> {
    const sites = await listConfiguredSites(this.db);
    this.log.debug("resync_started", { sites: sites.length });

    for (const site of sites) {
      if (!this.running) return;
      try {
        await this.syncSite(site);
      } catch (error) {
        this.log.warn("resync_site_failed", {
          pubkey: site.pubkey,
          identifier: site.identifier,
          ...errFields(error),
        });
      }
    }
  }

  /** Store a manifest event and queue it for deployment. */
  async ingest(event: NostrEvent): Promise<void> {
    if (!isSiteManifest(event)) return;

    const address = manifestAddress(event);
    if (!(await this.isConfigured(address))) {
      this.log.debug("manifest_ignored", { pubkey: event.pubkey, kind: event.kind });
      return;
    }

    const stored = await storeManifest(this.db, event);
    if (!stored) return;

    this.log.info("manifest_received", {
      event_id: event.id,
      pubkey: event.pubkey,
      identifier: address.identifier,
      created_at: event.created_at,
    });
    await writeDeployLog(this.db, event.id, "info", "manifest_received", {
      created_at: event.created_at,
    });

    this.enqueue(event.id);
  }

  /** Queue every version that isn't ready yet. */
  private async retryIncomplete(): Promise<void> {
    const sites = await listConfiguredSites(this.db);

    for (const site of sites) {
      const newest = await newestEvent(this.db, site);
      if (newest && newest.status !== "ready") this.enqueue(newest.event_id);
    }
  }

  private async isConfigured(address: NsiteAddress): Promise<boolean> {
    const host = await this.db
      .selectFrom("hosts")
      .select("id")
      .where("pubkey", "=", address.pubkey)
      .where("kind", "=", address.kind)
      .where("identifier", "=", address.identifier)
      .where("enabled", "=", 1)
      .executeTakeFirst();

    return host !== undefined;
  }

  private scheduleResync(): void {
    if (this.resyncTimer) clearInterval(this.resyncTimer);

    const interval = this.settings.get("resync_interval") * 1000;
    this.resyncTimer = setInterval(() => void this.resyncAll(), interval);
    // Don't hold the process open for a timer.
    this.resyncTimer.unref?.();

    this.log.debug("resync_scheduled", { interval_seconds: interval / 1000 });
  }

  private async restartSubscription(): Promise<void> {
    this.subscription?.abort();

    if (!this.running) return;

    const sites = await listConfiguredSites(this.db);
    if (!sites.length) {
      this.log.debug("subscription_idle", { reason: "no_hosts" });
      return;
    }

    const controller = new AbortController();
    this.subscription = controller;

    void this.runSubscription(sites, controller.signal);
  }

  private async runSubscription(sites: NsiteAddress[], signal: AbortSignal): Promise<void> {
    const authors = [...new Set(sites.map((site) => site.pubkey))];
    const kinds = [...new Set(sites.map((site) => site.kind))].filter((kind) => SITE_KINDS.includes(kind));

    // Watch the authors' own write relays too — that's where they publish.
    const hints = [...(await this.nostr.getAuthorsRelays(authors, { signal })).values()].flat();

    const relays = [...new Set([...this.nostr.relays, ...hints])];
    const since = Math.floor(Date.now() / 1000);

    this.log.info("subscription_started", {
      authors: authors.length,
      relays: relays.length,
    });

    try {
      for await (const msg of this.nostr.req([{ kinds, authors, since }], { signal, relays })) {
        if (msg[0] !== "EVENT") continue;
        try {
          await this.ingest(msg[2]);
        } catch (error) {
          this.log.error("ingest_failed", { event_id: msg[2].id, ...errFields(error) });
        }
      }
    } catch (error) {
      if (signal.aborted) return;
      this.log.warn("subscription_failed", errFields(error));
    }
  }

  /**
   * Queue a version for deployment, with a fresh run of attempts.
   *
   * Everything that calls this — a new manifest, a resync, a settings change —
   * is a reason to believe a deploy that gave up earlier could now succeed.
   */
  private enqueue(eventId: string): void {
    this.attempts.delete(eventId);
    this.queueDeploy(eventId, 0);
  }

  /** Queue a version, to be deployed no earlier than `after` (epoch ms). */
  private queueDeploy(eventId: string, after: number): void {
    const pending = this.queue.find((entry) => entry.eventId === eventId);

    if (pending) {
      // A fresh reason to deploy overtakes a retry still waiting out its backoff.
      pending.after = Math.min(pending.after, after);
    } else {
      this.queue.push({ eventId, after });
    }

    this.wake?.();

    this.pumping ??= this.pump().finally(() => {
      this.pumping = undefined;
    });
  }

  /**
   * Deploy queued versions one at a time.
   *
   * A version waiting out a retry's backoff holds up nothing: whatever is ready
   * to deploy goes first, and only an otherwise empty queue actually waits.
   */
  private async pump(): Promise<void> {
    while (this.running) {
      const eventId = this.takeReady();

      if (eventId === undefined) {
        const wait = this.nextWait();
        if (wait === undefined) return;
        await this.waitFor(wait);
        continue;
      }

      try {
        await this.deploy(eventId);
      } catch (error) {
        if (!this.running) {
          // Whatever went wrong is a consequence of shutting down, not of the
          // version itself, so leave it for the next run rather than failing it.
          await this.interrupt(eventId, 0);
          return;
        }
        this.log.error("deploy_crashed", { event_id: eventId, ...errFields(error) });
        await this.fail(eventId, `Deploy failed: ${message(error)}`);
      }
    }
  }

  /** Take the first queued version whose wait is over, if any. */
  private takeReady(): string | undefined {
    const index = this.queue.findIndex((entry) => entry.after <= Date.now());
    if (index === -1) return undefined;

    return this.queue.splice(index, 1)[0].eventId;
  }

  /** How long until the earliest queued version may deploy, in ms. */
  private nextWait(): number | undefined {
    if (!this.queue.length) return undefined;

    const soonest = Math.min(...this.queue.map((entry) => entry.after));
    return Math.max(0, soonest - Date.now());
  }

  /** Wait, unless something is queued sooner or the worker stops first. */
  private waitFor(ms: number): Promise<void> {
    const { signal } = this.controller;
    if (signal.aborted) return Promise.resolve();

    return new Promise<void>((resolve) => {
      const done = (): void => {
        clearTimeout(timer);
        signal.removeEventListener("abort", done);
        this.wake = undefined;
        resolve();
      };

      const timer = setTimeout(done, ms);
      // Don't hold the process open for a deploy that's only waiting.
      timer.unref?.();

      signal.addEventListener("abort", done, { once: true });
      this.wake = done;
    });
  }

  /**
   * Download every blob a version needs, then mark it ready.
   *
   * Only when this completes does the version become servable, so a host is
   * never left half-updated: it keeps serving the previous version until the
   * new one is fully present on disk.
   */
  private async deploy(eventId: string): Promise<void> {
    const row = await this.db.selectFrom("site_events").selectAll().where("event_id", "=", eventId).executeTakeFirst();

    if (!row || row.status === "ready") return;

    const event = parseStoredEvent(row);

    await this.db
      .updateTable("site_events")
      .set({ status: "fetching_blobs", error: null })
      .where("event_id", "=", eventId)
      .execute();

    // Paths come along because the store is keyed by hash and so can't tell
    // markup from a JPEG, while whether a blob is worth compressing is a
    // question about the file it is served as. A hash reached by several paths
    // gets compressed if any one of them wants it.
    const files = await this.db
      .selectFrom("site_files")
      .select(["sha256", "path"])
      .where("event_id", "=", eventId)
      .execute();

    const compressible = new Set<string>();
    for (const file of files) {
      if (isCompressible(file.path)) compressible.add(file.sha256);
    }

    const hashes = [...new Set(files.map((file) => file.sha256))];
    await writeDeployLog(this.db, eventId, "info", "deploy_started", {
      files: row.files_total,
      blobs: hashes.length,
    });

    const missing: string[] = [];
    for (const sha256 of hashes) {
      const size = await this.blobs.size(sha256);
      if (size === undefined) {
        missing.push(sha256);
      } else {
        await recordBlob(this.db, sha256, size);
        // Already on disk, but not necessarily compressed: it may have been
        // downloaded before npanel did this, or by a site that serves it under
        // a name that isn't worth compressing.
        if (compressible.has(sha256)) await this.compress(eventId, sha256);
        await recordVariantSizes(this.db, this.blobs, sha256);
      }
    }

    await this.setProgress(eventId, hashes.length - missing.length);

    if (!missing.length) {
      await this.finish(eventId, "ready");
      return;
    }

    const servers = await this.nostr.blobSources(event, this.settings.get("blossom_fallback_servers"), {
      signal: this.controller.signal,
    });

    if (!servers.length) {
      if (!this.running) {
        // The lookup was aborted by the shutdown, not answered with nothing.
        await this.interrupt(eventId, missing.length);
        return;
      }

      const reason =
        "No Blossom servers to download from. The manifest has no server tags and the author has no kind 10063 list — add fallback servers in Settings.";
      await writeDeployLog(this.db, eventId, "error", "no_blob_servers", { missing: missing.length });
      await this.fail(eventId, reason);
      return;
    }

    await writeDeployLog(this.db, eventId, "info", "downloading_blobs", {
      missing: missing.length,
      servers: servers.join(" "),
    });

    const { failed, abandoned } = await this.downloadAll(eventId, missing, servers, {
      compressible,
      alreadyReady: hashes.length - missing.length,
    });

    if (abandoned) {
      // Shutting down mid-deploy. Marking this ready would serve a site whose
      // files aren't all on disk, and a ready version is never retried.
      await this.interrupt(eventId, abandoned + failed.length);
      return;
    }

    if (failed.length) {
      const sample = failed.slice(0, 3).join(", ");
      const reason = `${failed.length} of ${hashes.length} blobs could not be downloaded from ${servers.length} server(s): ${sample}${failed.length > 3 ? ", …" : ""}`;
      await this.fail(eventId, reason);
      return;
    }

    await this.finish(eventId, "ready");
  }

  /**
   * Download missing blobs with bounded concurrency.
   *
   * Returns the blobs no server could supply, plus however many were never
   * attempted because the worker stopped — those are not failures, and a
   * version that has any of them must not be marked ready.
   */
  private async downloadAll(
    eventId: string,
    missing: string[],
    servers: string[],
    opts: { compressible: Set<string>; alreadyReady: number },
  ): Promise<{ failed: string[]; abandoned: number }> {
    const maxSize = this.settings.get("max_file_size");
    const queue = [...missing];
    const failed: string[] = [];
    let ready = opts.alreadyReady;

    const worker = async (): Promise<void> => {
      while (this.running) {
        const sha256 = queue.shift();
        if (!sha256) return;

        const ok = await this.downloadBlob(eventId, sha256, servers, {
          maxSize,
          compress: opts.compressible.has(sha256),
        });
        if (ok) {
          ready++;
          await this.setProgress(eventId, ready);
        } else {
          failed.push(sha256);
        }
      }
    };

    await Promise.all(Array.from({ length: Math.min(BLOB_CONCURRENCY, queue.length) }, worker));

    return { failed, abandoned: queue.length };
  }

  /**
   * Record a failed deploy, and queue another attempt if any are left.
   *
   * The version stays `failed` in the meantime — a host goes on serving the
   * version it already has — and a retry resumes rather than restarts, because
   * the blobs of earlier attempts are still on disk.
   *
   * Each retry waits twice as long as the last. Deploys fail because something
   * on the other side is unavailable, and asking again in the same second
   * rarely changes that.
   *
   * Attempts are counted per run, not for the lifetime of the version. Once a
   * run is spent the resync timer still re-queues the version later, which is
   * what lets a site recover on its own when the servers holding its blobs come
   * back hours after the deploy that needed them.
   */
  private async fail(eventId: string, reason: string): Promise<void> {
    const attempt = (this.attempts.get(eventId) ?? 0) + 1;
    const limit = this.settings.get("deploy_attempts");
    const again = attempt < limit && this.running;

    if (again) {
      this.attempts.set(eventId, attempt);
    } else {
      this.attempts.delete(eventId);
    }

    const backoff = this.retryBackoff * 2 ** (attempt - 1);

    let explanation = reason;
    if (limit > 1) {
      const next = again ? `Trying again in ${duration(backoff)}.` : "The next sync will try again.";
      explanation = `${terminate(reason)} Attempt ${attempt} of ${limit}. ${next}`;
    }

    await this.finish(eventId, "failed", explanation);

    if (again) {
      await writeDeployLog(this.db, eventId, "info", "deploy_retrying", {
        attempt: attempt + 1,
        of: limit,
        wait_ms: backoff,
      });
      this.queueDeploy(eventId, Date.now() + backoff);
    }
  }

  /**
   * Leave an interrupted deploy incomplete, so the next sync resumes it.
   *
   * A version only ever leaves `pending`/`fetching_blobs` when every blob is
   * accounted for; anything else would strand a half-downloaded version in a
   * state nothing retries.
   */
  private async interrupt(eventId: string, remaining: number): Promise<void> {
    this.attempts.delete(eventId);

    await this.db
      .updateTable("site_events")
      .set({
        status: "pending",
        error: "Deploy was interrupted while npanel was stopping. It resumes on the next sync.",
      })
      .where("event_id", "=", eventId)
      .execute();

    await writeDeployLog(this.db, eventId, "warn", "deploy_interrupted", { remaining });
    this.log.warn("deploy_interrupted", { event_id: eventId, remaining });
  }

  /** Try each server in turn for one blob. */
  private async downloadBlob(
    eventId: string,
    sha256: string,
    servers: string[],
    opts: { maxSize: number; compress: boolean },
  ): Promise<boolean> {
    for (const server of servers) {
      const url = blobUrl(server, sha256);
      try {
        const size = await this.blobs.download(url, sha256, {
          maxSize: opts.maxSize,
          signal: this.controller.signal,
        });
        await recordBlob(this.db, sha256, size);
        await writeDeployLog(this.db, eventId, "debug", "blob_downloaded", { sha256, server, size });
        if (opts.compress) await this.compress(eventId, sha256);
        await recordVariantSizes(this.db, this.blobs, sha256);
        return true;
      } catch (error) {
        const fields: LogFields = {
          sha256,
          server,
          reason: error instanceof BlobDownloadError ? error.reason : "error",
          status: error instanceof BlobDownloadError ? error.status : undefined,
          err_msg: message(error),
        };
        await writeDeployLog(this.db, eventId, "warn", "blob_download_failed", fields);
        this.log.debug("blob_download_failed", fields);
      }
    }

    await writeDeployLog(this.db, eventId, "error", "blob_unavailable", {
      sha256,
      servers: servers.length,
    });

    return false;
  }

  /**
   * Store compressed copies of a blob.
   *
   * Never fatal. The blob itself is downloaded and verified by the time this
   * runs, and a site serving it uncompressed is a slower site rather than a
   * broken one — failing the deploy over this would take the whole version
   * offline to save some bandwidth.
   */
  private async compress(eventId: string, sha256: string): Promise<void> {
    try {
      const written = await this.blobs.ensureVariants(sha256);
      if (written.length) {
        await writeDeployLog(this.db, eventId, "debug", "blob_compressed", { sha256, encodings: written.join(" ") });
      }
    } catch (error) {
      const fields: LogFields = { sha256, err_msg: message(error) };
      await writeDeployLog(this.db, eventId, "warn", "blob_compress_failed", fields);
      this.log.warn("blob_compress_failed", fields);
    }
  }

  private async setProgress(eventId: string, filesReady: number): Promise<void> {
    await this.db.updateTable("site_events").set({ files_ready: filesReady }).where("event_id", "=", eventId).execute();
  }

  private async finish(eventId: string, status: DeployStatus, error?: string): Promise<void> {
    const now = Math.floor(Date.now() / 1000);

    await this.db
      .updateTable("site_events")
      .set({
        status,
        error: error ?? null,
        ready_at: status === "ready" ? now : null,
      })
      .where("event_id", "=", eventId)
      .execute();

    if (status === "ready") {
      this.attempts.delete(eventId);
      this.log.info("deploy_ready", { event_id: eventId });
      await writeDeployLog(this.db, eventId, "info", "deploy_ready");

      // The newest ready version may just have changed, and a version is only
      // served through a build of it.
      const site = await this.db
        .selectFrom("site_events")
        .select(["pubkey", "kind", "identifier"])
        .where("event_id", "=", eventId)
        .executeTakeFirst();
      if (site) void this.builder.siteChanged(site);
    } else {
      this.log.error("deploy_failed", { event_id: eventId, err_msg: error });
      await writeDeployLog(this.db, eventId, "error", "deploy_failed", { err_msg: error });
    }

    await pruneDeployLog(this.db, eventId);
  }
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** A wait, as someone reading about it would say it. */
function duration(ms: number): string {
  const seconds = Math.max(1, Math.round(ms / 1000));
  return seconds === 1 ? "a second" : `${seconds} seconds`;
}

/** End a sentence, so another can be appended to it. */
function terminate(text: string): string {
  return /[.!?…]$/.test(text) ? text : `${text}.`;
}
