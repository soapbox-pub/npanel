import { Worker } from "node:worker_threads";
import type { Kysely } from "kysely";
import type { Database } from "../db/schema.ts";
import { type Logger, errFields } from "../log.ts";
import type { AnalyticsHost } from "./ingest.ts";
import type { FromWorker, ToWorker, WorkerConfig } from "./protocol.ts";

/** First and longest wait before starting a thread that died, in milliseconds. */
const RESTART_MIN = 1_000;
const RESTART_MAX = 60_000;

/** A thread that ran this long before dying is restarted without the backoff it had built up. */
const HEALTHY_AFTER = 60_000;

/** How long an event forwarded to the thread may take to be answered. */
const EVENT_TIMEOUT = 10_000;

export interface AnalyticsServiceDeps {
  db: Kysely<Database>;
  log: Logger;
  config: WorkerConfig;
}

interface Pending {
  resolve(response: Response): void;
  timer: ReturnType<typeof setTimeout>;
}

/**
 * Runs the analytics thread and keeps it running.
 *
 * The thread is started with the process, told which sites are counting
 * whenever that changes, and restarted when it dies — after a second, then
 * doubling, so a thread that dies at startup can't spin. Everything it is told
 * is sent again on each start, so a new thread never depends on what the last
 * one knew.
 *
 * It is never killed from outside. A worker thread that is terminated keeps
 * its listening socket bound for the life of the process, so a replacement
 * could never take the port; the thread closes its own port on the way out,
 * including when it fails.
 */
export class AnalyticsService {
  private readonly db: Kysely<Database>;
  private readonly log: Logger;
  private readonly config: WorkerConfig;

  private worker: Worker | undefined;
  private ready = false;
  private boundPort: number | undefined;
  private stopping = false;
  private restartDelay = RESTART_MIN;
  private restartTimer: ReturnType<typeof setTimeout> | undefined;
  private startedAt = 0;
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private readyWaiters: (() => void)[] = [];

  constructor(deps: AnalyticsServiceDeps) {
    this.db = deps.db;
    this.log = deps.log;
    this.config = deps.config;
  }

  /** Start the thread. Resolves once it is listening. */
  async start(): Promise<void> {
    this.spawn();
    await this.whenReady();
  }

  /** Stop the thread, letting it write what it has buffered first. */
  async stop(): Promise<void> {
    this.stopping = true;
    clearTimeout(this.restartTimer);

    const worker = this.worker;
    if (!worker) return;

    await new Promise<void>((resolve) => {
      worker.once("exit", () => resolve());
      this.post({ type: "stop" });
    });
  }

  /** The port events are received on, once the thread is listening. */
  get port(): number | undefined {
    return this.boundPort;
  }

  /** Tell the thread which sites are counting. Call after any host changes. */
  async hostsChanged(): Promise<void> {
    if (!this.ready) return;
    this.post({ type: "hosts", hosts: await this.hosts() });
  }

  /**
   * Record an event that reached the main thread rather than the analytics
   * port: a visit over Tor, whose listener has no proxy in front of it, or a
   * deployment whose proxy doesn't route the path.
   *
   * The body is read and handed over whole — a few hundred bytes — and the
   * thread's answer returned as it gave it.
   */
  async event(request: Request): Promise<Response> {
    if (!this.ready) {
      return new Response("Analytics are starting. Try again in a moment.", {
        status: 503,
        headers: { "access-control-allow-origin": "*", "retry-after": "5" },
      });
    }

    const id = this.nextId++;
    const body = request.method === "POST" ? await request.text() : "";

    return await new Promise<Response>((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        resolve(new Response("Analytics took too long to answer.", { status: 504 }));
      }, EVENT_TIMEOUT);

      this.pending.set(id, { resolve, timer });

      this.post({
        type: "event",
        id,
        method: request.method,
        body,
        headers: {
          userAgent: request.headers.get("user-agent") ?? "",
          forwardedFor: request.headers.get("x-forwarded-for"),
          realIp: request.headers.get("x-real-ip"),
        },
      });
    });
  }

  private spawn(): void {
    this.ready = false;
    this.startedAt = Date.now();

    const worker = new Worker(new URL("./worker.ts", import.meta.url), { workerData: this.config });
    this.worker = worker;

    worker.on("message", (message: FromWorker) => void this.receive(message));

    worker.on("error", (error) => {
      this.log.error("analytics_thread_error", errFields(error));
    });

    worker.on("exit", (code) => {
      if (this.worker !== worker) return;
      this.worker = undefined;
      this.ready = false;

      for (const [id, pending] of this.pending) {
        clearTimeout(pending.timer);
        pending.resolve(new Response("Analytics restarted before answering.", { status: 503 }));
        this.pending.delete(id);
      }

      if (this.stopping) return;

      if (Date.now() - this.startedAt > HEALTHY_AFTER) this.restartDelay = RESTART_MIN;
      this.log.error("analytics_thread_exited", { code, restart_in_ms: this.restartDelay });

      this.restartTimer = setTimeout(() => this.spawn(), this.restartDelay);
      this.restartDelay = Math.min(this.restartDelay * 2, RESTART_MAX);
    });
  }

  private async receive(message: FromWorker): Promise<void> {
    switch (message.type) {
      case "ready":
        this.ready = true;
        this.boundPort = message.port;
        this.post({ type: "hosts", hosts: await this.hosts() });
        for (const resolve of this.readyWaiters.splice(0)) resolve();
        break;

      case "event": {
        const pending = this.pending.get(message.id);
        if (!pending) return;
        this.pending.delete(message.id);
        clearTimeout(pending.timer);

        const { status, headers, body } = message.response;
        pending.resolve(new Response(body || null, { status, headers }));
        break;
      }
    }
  }

  private whenReady(): Promise<void> {
    if (this.ready) return Promise.resolve();
    return new Promise((resolve) => this.readyWaiters.push(resolve));
  }

  private post(message: ToWorker): void {
    this.worker?.postMessage(message);
  }

  /** Every host, and whether events for it are kept. */
  private async hosts(): Promise<AnalyticsHost[]> {
    const rows = await this.db.selectFrom("hosts").select(["id", "hostname", "enabled", "analytics"]).execute();

    return rows.map((row) => ({
      id: row.id,
      hostname: row.hostname,
      counting: row.enabled === 1 && row.analytics === 1,
    }));
  }
}
