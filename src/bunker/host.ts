import { Worker } from "node:worker_threads";
import { type Logger, errFields } from "../log.ts";
import type { LogLevel } from "../log.ts";
import type { BunkerApi, BunkerGateway, BunkerTestResult, MintedConnection, PairingStatus } from "./api.ts";
import type { BunkerWorkerData } from "./worker.ts";

/** What to tell someone whose deployment can't do this yet. */
export const BUNKER_UNAVAILABLE =
  "This server has no SECRET_KEY set, so it can't hold signer credentials. Set one and restart npanel.";

/** How long a call may wait, above the longest the worker itself will wait. */
const CALL_TIMEOUT = 130_000;

const FIRST_RESTART_DELAY = 1_000;
const MAX_RESTART_DELAY = 60_000;
/** Uptime after which a crash is treated as a fresh problem, not a loop. */
const HEALTHY_UPTIME = 60_000;

export interface BunkerHostDeps {
  /** From `SECRET_KEY`. Undefined leaves the whole feature switched off. */
  secretKey: string | undefined;
  databasePath: string;
  dashboardHost: string;
  logLevel: LogLevel;
  log: Logger;
}

interface Pending {
  resolve(value: unknown): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
}

type Reply = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: string };

/**
 * The main thread's handle on the bunker gateway.
 *
 * Every method is a message to another thread, and every failure mode of that
 * thread — a crash, a hang, a runtime that can't spawn it at all — ends here as
 * a rejected promise on a bunker route. Nothing it does can reach the code
 * path that serves sites, which is the entire reason it isn't just a class the
 * API calls directly.
 */
export class BunkerHost implements BunkerGateway {
  /** Whether this deployment can hold credentials at all. */
  readonly available: boolean;

  private worker: Worker | undefined;
  private pending = new Map<number, Pending>();
  private nextId = 1;
  /** Calls in flight. While there are any, the worker holds the loop open. */
  private waiting = 0;
  private stopped = false;
  private restartDelay = FIRST_RESTART_DELAY;
  private spawnedAt = 0;
  private restartTimer: ReturnType<typeof setTimeout> | undefined;

  constructor(private readonly deps: BunkerHostDeps) {
    this.available = deps.secretKey !== undefined;
  }

  start(): void {
    if (!this.available || this.worker || this.stopped) return;
    this.spawn();
  }

  async stop(): Promise<void> {
    this.stopped = true;

    if (this.restartTimer) clearTimeout(this.restartTimer);
    const worker = this.worker;
    if (!worker) return;

    // Ask first so the worker closes its database connection, then insist:
    // a wedged gateway must not be able to hold up the whole shutdown.
    await this.call("stop").catch(() => {});
    this.worker = undefined;
    await worker.terminate();
  }

  reload(): Promise<void> {
    return this.call("reload");
  }

  startPairing(input: { bunkerId: number; uri?: string }): Promise<PairingStatus> {
    return this.call("startPairing", input);
  }

  pairingStatus(bunkerId: number): Promise<PairingStatus> {
    return this.call("pairingStatus", bunkerId);
  }

  cancelPairing(bunkerId: number): Promise<void> {
    return this.call("cancelPairing", bunkerId);
  }

  testBunker(bunkerId: number): Promise<BunkerTestResult> {
    return this.call("testBunker", bunkerId);
  }

  mintConnection(input: {
    bunkerId: number;
    name: string;
    permissions: string[];
    createdBy: string;
    uri?: string;
  }): Promise<MintedConnection> {
    return this.call("mintConnection", input);
  }

  private call<K extends keyof BunkerApi>(
    method: K,
    ...args: Parameters<BunkerApi[K]>
  ): Promise<Awaited<ReturnType<BunkerApi[K]>>> {
    if (!this.available) {
      return Promise.reject(new Error(BUNKER_UNAVAILABLE));
    }

    // A crashed worker is restarting on a timer; a call arriving meanwhile
    // brings it back early rather than failing for no reason.
    if (!this.worker && !this.stopped) this.spawn();

    const worker = this.worker;
    if (!worker) {
      return Promise.reject(new Error("The bunker gateway isn't running."));
    }

    const id = this.nextId++;

    // An unref'd worker doesn't keep the process alive, which is what we want
    // of an idle gateway — but a call in flight is exactly the case where
    // nothing else may be holding the loop open, and a runtime that tears the
    // thread down there turns a slow answer into a lost one.
    if (this.waiting++ === 0) worker.ref();

    return new Promise<Awaited<ReturnType<BunkerApi[K]>>>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.forget(id);
        reject(new Error("The bunker gateway didn't answer in time."));
      }, CALL_TIMEOUT);
      timer.unref?.();

      this.pending.set(id, {
        resolve: resolve as (value: unknown) => void,
        reject,
        timer,
      });

      worker.postMessage({ id, method, args });
    });
  }

  /** Drop a call, releasing its hold on the event loop. */
  private forget(id: number): Pending | undefined {
    const pending = this.pending.get(id);
    if (!pending) return undefined;

    this.pending.delete(id);
    clearTimeout(pending.timer);

    if (--this.waiting === 0) this.worker?.unref();

    return pending;
  }

  private spawn(): void {
    const { secretKey, databasePath, dashboardHost, logLevel, log } = this.deps;
    if (secretKey === undefined) return;

    if (this.restartTimer) {
      clearTimeout(this.restartTimer);
      this.restartTimer = undefined;
    }

    const workerData: BunkerWorkerData = { databasePath, secretKey, dashboardHost, logLevel };

    let worker: Worker;
    try {
      worker = new Worker(new URL("./worker.ts", import.meta.url), { workerData });
    } catch (error) {
      log.error("bunker_worker_spawn_failed", errFields(error));
      this.scheduleRestart();
      return;
    }

    // An idle gateway shouldn't keep the process alive — but a respawn may be
    // happening because a call arrived, and that one is still waiting.
    if (this.waiting > 0) {
      worker.ref();
    } else {
      worker.unref();
    }

    worker.on("message", (reply: Reply) => this.settle(reply));
    worker.on("error", (error) => {
      log.error("bunker_worker_failed", errFields(error));
    });
    worker.on("exit", (code) => this.onExit(code));

    this.worker = worker;
    this.spawnedAt = Date.now();

    log.info("bunker_worker_started", {});
  }

  private settle(reply: Reply): void {
    const pending = this.forget(reply.id);
    if (!pending) return;

    if (reply.ok) {
      pending.resolve(reply.value);
    } else {
      pending.reject(new Error(reply.error));
    }
  }

  private onExit(code: number): void {
    if (this.stopped) return;

    this.deps.log.warn("bunker_worker_exited", { code });
    this.worker = undefined;

    // Nothing will ever answer these now, and a caller waiting out the full
    // timeout for a thread that is already gone helps nobody.
    for (const id of [...this.pending.keys()]) {
      this.forget(id)?.reject(new Error("The bunker gateway restarted. Try again."));
    }

    if (Date.now() - this.spawnedAt > HEALTHY_UPTIME) {
      this.restartDelay = FIRST_RESTART_DELAY;
    }

    this.scheduleRestart();
  }

  private scheduleRestart(): void {
    if (this.stopped || this.restartTimer) return;

    const delay = this.restartDelay;
    this.restartDelay = Math.min(this.restartDelay * 2, MAX_RESTART_DELAY);

    this.restartTimer = setTimeout(() => {
      this.restartTimer = undefined;
      this.spawn();
    }, delay);
    this.restartTimer.unref?.();
  }
}
