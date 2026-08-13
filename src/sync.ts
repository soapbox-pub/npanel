import type { NostrEvent } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import { BlobDownloadError, type BlobStore } from "./blobs.ts";
import type { Database, DeployStatus } from "./db/schema.ts";
import { type LogFields, type Logger, errFields } from "./log.ts";
import { isSiteManifest, manifestAddress } from "./manifest.ts";
import { type NostrService, blobUrl } from "./nostr.ts";
import { type NsiteAddress, SITE_KINDS } from "./nsite-address.ts";
import type { SettingsService } from "./settings.ts";
import { listConfiguredSites, newestEvent, parseStoredEvent, storeManifest } from "./sites.ts";

/** Blobs downloaded in parallel within one deploy. */
const BLOB_CONCURRENCY = 4;

/** Deploy log entries kept per version. Older entries are pruned. */
const DEPLOY_LOG_LIMIT = 500;

export interface SyncWorkerDeps {
  db: Kysely<Database>;
  blobs: BlobStore;
  nostr: NostrService;
  settings: SettingsService;
  log: Logger;
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
 *    version is allowed to serve.
 *
 * Every step also writes to the `deploy_log` table, which the dashboard shows
 * per version — a failed deploy explains itself without touching the server.
 */
export class SyncWorker {
  private readonly db: Kysely<Database>;
  private readonly blobs: BlobStore;
  private readonly nostr: NostrService;
  private readonly settings: SettingsService;
  private readonly log: Logger;

  private controller = new AbortController();
  private subscription: AbortController | undefined;
  private resyncTimer: ReturnType<typeof setInterval> | undefined;
  private unsubscribeSettings: (() => void) | undefined;

  private queue: string[] = [];
  private queued = new Set<string>();
  /** Attempts already spent on a version, while it is being retried. */
  private attempts = new Map<string, number>();
  private pumping: Promise<void> | undefined;
  private resyncing: Promise<void> | undefined;
  private resyncAgain = false;
  private running = false;

  constructor(deps: SyncWorkerDeps) {
    this.db = deps.db;
    this.blobs = deps.blobs;
    this.nostr = deps.nostr;
    this.settings = deps.settings;
    this.log = deps.log;
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
    await this.writeDeployLog(event.id, "info", "manifest_received", {
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
    const hints: string[] = [];
    for (const author of authors) {
      try {
        hints.push(...(await this.nostr.getAuthorRelays(author, { signal })));
      } catch {
        // A missing relay list just means we watch the configured relays.
      }
    }

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
    this.queueDeploy(eventId);
  }

  private queueDeploy(eventId: string): void {
    if (this.queued.has(eventId)) return;

    this.queued.add(eventId);
    this.queue.push(eventId);

    this.pumping ??= this.pump().finally(() => {
      this.pumping = undefined;
    });
  }

  /** Deploy queued versions one at a time. */
  private async pump(): Promise<void> {
    while (this.running) {
      const eventId = this.queue.shift();
      if (!eventId) return;
      this.queued.delete(eventId);

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

    const files = await this.db
      .selectFrom("site_files")
      .select("sha256")
      .where("event_id", "=", eventId)
      .distinct()
      .execute();

    const hashes = files.map((file) => file.sha256);
    await this.writeDeployLog(eventId, "info", "deploy_started", {
      files: row.files_total,
      blobs: hashes.length,
    });

    const missing: string[] = [];
    for (const sha256 of hashes) {
      const size = await this.blobs.size(sha256);
      if (size === undefined) {
        missing.push(sha256);
      } else {
        await this.recordBlob(sha256, size);
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
      await this.writeDeployLog(eventId, "error", "no_blob_servers", { missing: missing.length });
      await this.fail(eventId, reason);
      return;
    }

    await this.writeDeployLog(eventId, "info", "downloading_blobs", {
      missing: missing.length,
      servers: servers.join(" "),
    });

    const { failed, abandoned } = await this.downloadAll(eventId, missing, servers, hashes.length - missing.length);

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
    alreadyReady: number,
  ): Promise<{ failed: string[]; abandoned: number }> {
    const maxSize = this.settings.get("max_file_size");
    const queue = [...missing];
    const failed: string[] = [];
    let ready = alreadyReady;

    const worker = async (): Promise<void> => {
      while (this.running) {
        const sha256 = queue.shift();
        if (!sha256) return;

        const ok = await this.downloadBlob(eventId, sha256, servers, maxSize);
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

    let explanation = reason;
    if (limit > 1) {
      const next = again ? "Trying again." : "The next sync will try again.";
      explanation = `${terminate(reason)} Attempt ${attempt} of ${limit}. ${next}`;
    }

    await this.finish(eventId, "failed", explanation);

    if (again) {
      await this.writeDeployLog(eventId, "info", "deploy_retrying", { attempt: attempt + 1, of: limit });
      this.queueDeploy(eventId);
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

    await this.writeDeployLog(eventId, "warn", "deploy_interrupted", { remaining });
    this.log.warn("deploy_interrupted", { event_id: eventId, remaining });
  }

  /** Try each server in turn for one blob. */
  private async downloadBlob(eventId: string, sha256: string, servers: string[], maxSize: number): Promise<boolean> {
    for (const server of servers) {
      const url = blobUrl(server, sha256);
      try {
        const size = await this.blobs.download(url, sha256, { maxSize, signal: this.controller.signal });
        await this.recordBlob(sha256, size);
        await this.writeDeployLog(eventId, "debug", "blob_downloaded", { sha256, server, size });
        return true;
      } catch (error) {
        const fields: LogFields = {
          sha256,
          server,
          reason: error instanceof BlobDownloadError ? error.reason : "error",
          status: error instanceof BlobDownloadError ? error.status : undefined,
          err_msg: message(error),
        };
        await this.writeDeployLog(eventId, "warn", "blob_download_failed", fields);
        this.log.debug("blob_download_failed", fields);
      }
    }

    await this.writeDeployLog(eventId, "error", "blob_unavailable", {
      sha256,
      servers: servers.length,
    });

    return false;
  }

  private async recordBlob(sha256: string, size: number): Promise<void> {
    const now = Math.floor(Date.now() / 1000);
    await this.db
      .insertInto("blobs")
      .values({ sha256, size, seen_at: now })
      .onConflict((oc) => oc.column("sha256").doUpdateSet({ seen_at: now, size }))
      .execute();
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
      await this.writeDeployLog(eventId, "info", "deploy_ready");
    } else {
      this.log.error("deploy_failed", { event_id: eventId, err_msg: error });
      await this.writeDeployLog(eventId, "error", "deploy_failed", { err_msg: error });
    }

    await this.pruneDeployLog(eventId);
  }

  /** Append to the deploy log the dashboard shows for this version. */
  private async writeDeployLog(
    eventId: string,
    level: "debug" | "info" | "warn" | "error",
    msg: string,
    fields?: LogFields,
  ): Promise<void> {
    const cleaned = fields
      ? Object.fromEntries(Object.entries(fields).filter(([, value]) => value !== undefined))
      : undefined;

    await this.db
      .insertInto("deploy_log")
      .values({
        event_id: eventId,
        level,
        msg,
        fields: cleaned && Object.keys(cleaned).length ? JSON.stringify(cleaned) : null,
        created_at: Math.floor(Date.now() / 1000),
      })
      .execute();
  }

  private async pruneDeployLog(eventId: string): Promise<void> {
    const cutoff = await this.db
      .selectFrom("deploy_log")
      .select("id")
      .where("event_id", "=", eventId)
      .orderBy("id", "desc")
      .offset(DEPLOY_LOG_LIMIT)
      .limit(1)
      .executeTakeFirst();

    if (!cutoff) return;

    await this.db.deleteFrom("deploy_log").where("event_id", "=", eventId).where("id", "<=", cutoff.id).execute();
  }
}

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** End a sentence, so another can be appended to it. */
function terminate(text: string): string {
  return /[.!?…]$/.test(text) ? text : `${text}.`;
}
