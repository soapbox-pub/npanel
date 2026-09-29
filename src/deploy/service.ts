import { Worker } from "node:worker_threads";
import { type Logger, errFields } from "../log.ts";
import type { NsiteAddress } from "../nsite-address.ts";
import type { ServeCache } from "../serve-cache.ts";
import type { SettingsService } from "../settings.ts";
import type { Deployments } from "./deployer.ts";
import type { DeployConfig, FromDeploy, ToDeploy } from "./protocol.ts";

/** First and longest wait before starting a thread that died, in milliseconds. */
const RESTART_MIN = 1_000;
const RESTART_MAX = 60_000;

/** A thread that ran this long before dying is restarted without the backoff it had built up. */
const HEALTHY_AFTER = 60_000;

export interface DeployServiceDeps {
  settings: SettingsService;
  /** Cleared whenever the thread says a build went live. */
  serveCache: ServeCache;
  log: Logger;
  config: DeployConfig;
}

/**
 * Runs the deploy thread and keeps it running.
 *
 * Messages sent while the thread is starting are held and delivered once it
 * is ready, and so is a snapshot of every setting, since one may have been
 * written between the thread reading them and saying so. A thread that dies
 * is started again after a second, then doubling, so one that dies at startup
 * can't spin — and what it was asked before it died needs no replaying,
 * because a new thread reconciles everything as it starts.
 */
export class DeployService implements Deployments {
  private readonly settings: SettingsService;
  private readonly serveCache: ServeCache;
  private readonly log: Logger;
  private readonly config: DeployConfig;

  private worker: Worker | undefined;
  private ready = false;
  private held: ToDeploy[] = [];
  private stopping = false;
  private restartDelay = RESTART_MIN;
  private restartTimer: ReturnType<typeof setTimeout> | undefined;
  private startedAt = 0;
  private readyWaiters: (() => void)[] = [];
  private unsubscribe: (() => void) | undefined;

  constructor(deps: DeployServiceDeps) {
    this.settings = deps.settings;
    this.serveCache = deps.serveCache;
    this.log = deps.log;
    this.config = deps.config;
  }

  /** Start the thread. Resolves once it has started syncing. */
  async start(): Promise<void> {
    this.unsubscribe = this.settings.onChange(() => this.post(this.snapshot()));

    this.spawn();
    await this.whenReady();
  }

  /** Stop the thread, letting any deploy in progress leave itself resumable. */
  async stop(): Promise<void> {
    this.stopping = true;
    this.unsubscribe?.();
    clearTimeout(this.restartTimer);

    const worker = this.worker;
    if (!worker) return;

    await new Promise<void>((resolve) => {
      worker.once("exit", () => resolve());
      // Past the hold: a thread still starting stops as soon as it has.
      worker.postMessage({ type: "stop" } satisfies ToDeploy);
    });
  }

  hostsChanged(): void {
    this.post({ type: "hosts-changed" });
  }

  rebuild(hostIds: number[]): void {
    this.post({ type: "rebuild", hostIds });
  }

  syncSite(address: NsiteAddress): void {
    this.post({ type: "sync-site", address });
  }

  private spawn(): void {
    this.ready = false;
    this.startedAt = Date.now();

    const worker = new Worker(new URL("./worker.ts", import.meta.url), { workerData: this.config });
    this.worker = worker;

    worker.on("message", (message: FromDeploy) => this.receive(message));

    worker.on("error", (error) => {
      this.log.error("deploy_thread_error", errFields(error));
    });

    worker.on("exit", (code) => {
      if (this.worker !== worker) return;
      this.worker = undefined;
      this.ready = false;

      if (this.stopping) return;

      if (Date.now() - this.startedAt > HEALTHY_AFTER) this.restartDelay = RESTART_MIN;
      this.log.error("deploy_thread_exited", { code, restart_in_ms: this.restartDelay });

      this.restartTimer = setTimeout(() => this.spawn(), this.restartDelay);
      this.restartDelay = Math.min(this.restartDelay * 2, RESTART_MAX);
    });
  }

  private receive(message: FromDeploy): void {
    switch (message.type) {
      case "ready":
        this.ready = true;
        this.worker?.postMessage(this.snapshot());
        for (const held of this.held.splice(0)) this.worker?.postMessage(held);
        for (const resolve of this.readyWaiters.splice(0)) resolve();
        break;

      case "serving-changed":
        this.serveCache.clear();
        break;
    }
  }

  private whenReady(): Promise<void> {
    if (this.ready) return Promise.resolve();
    return new Promise((resolve) => this.readyWaiters.push(resolve));
  }

  private post(message: ToDeploy): void {
    if (this.ready && this.worker) {
      this.worker.postMessage(message);
    } else {
      this.held.push(message);
    }
  }

  private snapshot(): ToDeploy {
    return { type: "settings", values: this.settings.all() };
  }
}
