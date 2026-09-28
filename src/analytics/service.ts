import { Worker } from "node:worker_threads";
import type { Kysely } from "kysely";
import type { Database } from "../db/schema.ts";
import { type Logger, errFields } from "../log.ts";
import type { SettingsService } from "../settings.ts";
import type { AnalyticsHost } from "./ingest.ts";
import type { FromWorker, ToWorker, WorkerConfig, WorkerRequest, WorkerResponse } from "./protocol.ts";
import type { QueryHost } from "./query/engine.ts";

/** First and longest wait before starting a thread that died, in milliseconds. */
const RESTART_MIN = 1_000;
const RESTART_MAX = 60_000;

/** A thread that ran this long before dying is restarted without the backoff it had built up. */
const HEALTHY_AFTER = 60_000;

/** How long an event forwarded to the thread may take to be answered. */
const EVENT_TIMEOUT = 10_000;

/** How long a dashboard query may take. Long, since a year of a busy site is a lot of rows. */
const QUERY_TIMEOUT = 60_000;

const JSON_HEADERS = { "content-type": "application/json" };

export interface AnalyticsServiceDeps {
  db: Kysely<Database>;
  settings: SettingsService;
  log: Logger;
  config: WorkerConfig;
}

interface Pending {
  resolve(response: WorkerResponse): void;
  timer: ReturnType<typeof setTimeout>;
}

/**
 * Runs the analytics thread and keeps it running.
 *
 * The thread is started with the process, told which sites are counting and
 * which time zone to report in whenever either changes, and restarted when it
 * dies — after a second, then doubling, so a thread that dies at startup can't
 * spin. Everything it is told is sent again on each start, so a new thread
 * never depends on what the last one knew.
 *
 * It is never killed from outside. A worker thread that is terminated keeps
 * its listening socket bound for the life of the process, so a replacement
 * could never take the port; the thread closes its own port on the way out,
 * including when it fails.
 */
export class AnalyticsService {
  private readonly db: Kysely<Database>;
  private readonly settings: SettingsService;
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
  private unsubscribe: (() => void) | undefined;

  constructor(deps: AnalyticsServiceDeps) {
    this.db = deps.db;
    this.settings = deps.settings;
    this.log = deps.log;
    this.config = deps.config;
  }

  /** Start the thread. Resolves once it is listening. */
  async start(): Promise<void> {
    this.unsubscribe = this.settings.onChange((keys) => {
      if (keys.includes("analytics_timezone")) this.post(this.timezone());
    });

    this.spawn();
    await this.whenReady();
  }

  /** Stop the thread, letting it write what it has buffered first. */
  async stop(): Promise<void> {
    this.stopping = true;
    this.unsubscribe?.();
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
    const response = await this.call(
      {
        type: "event",
        method: request.method,
        body: request.method === "POST" ? await request.text() : "",
        headers: {
          userAgent: request.headers.get("user-agent") ?? "",
          forwardedFor: request.headers.get("x-forwarded-for"),
          realIp: request.headers.get("x-real-ip"),
        },
      },
      EVENT_TIMEOUT,
    );

    // An event the thread couldn't take still reaches a browser that is owed
    // the permission to have sent it.
    return new Response(response.body || null, {
      status: response.status,
      headers: { "access-control-allow-origin": "*", ...response.headers },
    });
  }

  /** Answer a dashboard query, Plausible's `POST /api/stats/:domain/query`. */
  query(host: QueryHost, body: unknown): Promise<WorkerResponse> {
    return this.call({ type: "query", host, body }, QUERY_TIMEOUT);
  }

  /** Visitors in the last five minutes, Plausible's `GET /api/stats/:domain/current-visitors`. */
  currentVisitors(host: QueryHost): Promise<WorkerResponse> {
    return this.call({ type: "current-visitors", host }, QUERY_TIMEOUT);
  }

  /** Each site's visitors over the last 24 hours, hour by hour, for the list of sites. */
  sparklines(hosts: QueryHost[]): Promise<WorkerResponse> {
    return this.call({ type: "sparklines", hosts }, QUERY_TIMEOUT);
  }

  /** Filter autocomplete, Plausible's `GET /api/stats/:domain/suggestions/:filter_name`. */
  suggestions(host: QueryHost, filter: string, params: Record<string, string>): Promise<WorkerResponse> {
    return this.call({ type: "suggestions", host, filter, params }, QUERY_TIMEOUT);
  }

  /** Ask the thread something, and wait for its answer or give up. */
  private call(request: WorkerRequest, timeout: number): Promise<WorkerResponse> {
    if (!this.ready) {
      return Promise.resolve(failure(503, "Analytics are starting. Try again in a moment."));
    }

    const id = this.nextId++;

    return new Promise<WorkerResponse>((resolve) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        resolve(failure(504, "Analytics took too long to answer. Try a shorter date range."));
      }, timeout);

      this.pending.set(id, { resolve, timer });
      this.post({ type: "request", id, request });
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
        pending.resolve(failure(503, "Analytics restarted before answering. Try again in a moment."));
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
        this.boundPort = message.port;
        this.post(this.timezone());
        this.post({ type: "hosts", hosts: await this.hosts() });
        this.ready = true;
        for (const resolve of this.readyWaiters.splice(0)) resolve();
        break;

      case "reply": {
        const pending = this.pending.get(message.id);
        if (!pending) return;
        this.pending.delete(message.id);
        clearTimeout(pending.timer);
        pending.resolve(message.response);
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

  private timezone(): ToWorker {
    return { type: "settings", timezone: this.settings.get("analytics_timezone") };
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

function failure(status: number, error: string): WorkerResponse {
  return { status, headers: JSON_HEADERS, body: JSON.stringify({ error }) };
}
