import type { Kysely } from "kysely";
import type { BlobStore } from "../blobs.ts";
import { HostBuilder } from "../builds.ts";
import type { Database } from "../db/schema.ts";
import { type Logger, errFields } from "../log.ts";
import type { NostrService } from "../nostr.ts";
import type { NsiteAddress } from "../nsite-address.ts";
import type { SettingsService } from "../settings.ts";
import { SyncWorker } from "../sync.ts";

/**
 * What the rest of npanel asks of deployments, wherever they run.
 *
 * Every method is a notification. None waits for the work it starts, which
 * can take minutes, and none has to be delivered to be correct: sync and
 * builds are reconciled rather than tracked, so the deploy thread starting up
 * does everything a missed call would have asked for.
 */
export interface Deployments {
  /** Hosts were added, removed, retargeted or toggled: resubscribe, and fetch what's new. */
  hostsChanged(): void;
  /** Reconcile these hosts' builds, since something they're made of may have changed. */
  rebuild(hostIds: number[]): void;
  /** Fetch a site's newest manifest now, and deploy it if it isn't ready. */
  syncSite(address: NsiteAddress): void;
}

export interface DeployerDeps {
  db: Kysely<Database>;
  blobs: BlobStore;
  nostr: NostrService;
  settings: SettingsService;
  log: Logger;
  /** Told whenever what a host serves changes, so serving's cache can be cleared. */
  servingChanged(): void;
  /** Overrides the retry backoff of deploys and builds, so tests don't wait one out. */
  retryBackoff?: number;
}

/**
 * Sync and builds, wired together: everything the deploy thread does.
 *
 * The thread runs one of these, and tests run one in-process, so both drive
 * the same composition. Nothing here knows which thread it is on.
 */
export class Deployer implements Deployments {
  readonly builder: HostBuilder;
  readonly sync: SyncWorker;
  private readonly log: Logger;

  constructor(deps: DeployerDeps) {
    const { db, blobs, nostr, settings, log, servingChanged, retryBackoff } = deps;

    this.log = log;
    this.builder = new HostBuilder({ db, blobs, settings, servingChanged, log, retryBackoff });
    this.sync = new SyncWorker({ db, blobs, nostr, settings, builder: this.builder, log, retryBackoff });
  }

  /**
   * Builds first, so a deploy that finishes early finds nothing stale left to
   * clear. Every host is reconciled in the background; one whose build is
   * current costs a hash.
   */
  async start(): Promise<void> {
    await this.builder.start();
    await this.sync.start();
  }

  async stop(): Promise<void> {
    await this.sync.stop();
    await this.builder.stop();
  }

  hostsChanged(): void {
    void this.sync.hostsChanged();
  }

  rebuild(hostIds: number[]): void {
    void this.builder.hostsChanged(hostIds);
  }

  syncSite(address: NsiteAddress): void {
    // Asked for by someone pressing a button, with nobody waiting on the
    // answer: the dashboard polls the host for how it went.
    this.sync.syncSite(address).catch((error: unknown) => {
      this.log.warn("sync_site_failed", {
        pubkey: address.pubkey,
        identifier: address.identifier,
        ...errFields(error),
      });
    });
  }
}
