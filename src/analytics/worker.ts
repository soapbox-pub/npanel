import { join } from "node:path";
import process from "node:process";
import { parentPort, workerData } from "node:worker_threads";
import { openSqlite } from "../db/sqlite.ts";
import { Logger, errFields } from "../log.ts";
import { Geo } from "./geo.ts";
import { type AnalyticsHost, Ingest } from "./ingest.ts";
import type { FromWorker, SenderHeaders, ToWorker, WorkerConfig, WorkerRequest, WorkerResponse } from "./protocol.ts";
import { QueryEngine } from "./query/engine.ts";
import { Salts } from "./salts.ts";
import { SESSION_TIMEOUT, SessionCache } from "./sessions.ts";
import { Stats } from "./stats.ts";
import { AnalyticsStore } from "./store.ts";
import { EVENT_PATH } from "./tracker.ts";
import { UserAgents } from "./user-agent.ts";

/**
 * The analytics thread: npanel's Plausible server.
 *
 * It runs in a worker so that nothing it does competes with serving sites for
 * the main thread. Events arrive on a port of its own, which the proxy routes
 * `/.npanel/api/event` to on every site, and are recorded here without the
 * main thread seeing a byte of them. The main thread sends it which sites are
 * counting, and asks it the dashboard's questions over the message channel.
 *
 * The thread owns everything it touches — its own database file, its own
 * caches — and shares no memory with the main thread, so the worst a bug here
 * can do is stop the analytics. An uncaught error closes the port and exits
 * the thread, and the main thread starts a new one.
 */

/**
 * The slice of Bun's server API this file uses, declared structurally for the
 * same reason `src/tls-check.ts` declares it.
 */
declare const Bun: {
  serve(options: {
    port: number;
    hostname: string;
    maxRequestBodySize: number;
    fetch(request: Request, server: { requestIP(request: Request): { address: string } | null }): Promise<Response>;
    error(error: Error): Response;
  }): { readonly port: number; stop(closeActiveConnections?: boolean): Promise<void> };
};

/** How often buffered events are written. */
const FLUSH_EVERY = 2_000;

/** How often expired sessions are forgotten. */
const SWEEP_EVERY = 60_000;

/**
 * The largest event accepted. A real one is a few hundred bytes; this leaves
 * room for props without letting anyone post megabytes at the thread.
 */
const MAX_BODY = 64 * 1024;

const CORS = {
  "access-control-allow-origin": "*",
  "access-control-allow-methods": "POST, OPTIONS",
  "access-control-allow-headers": "content-type",
};

const config = workerData as WorkerConfig;
const port = parentPort;
if (!port) throw new Error("src/analytics/worker.ts runs as a worker thread, started by AnalyticsService.");

const log = new Logger(config.logLevel);
const store = new AnalyticsStore(await openSqlite(join(config.dataDir, "analytics.sqlite3")));
const geo = new Geo(join(config.dataDir, "geo"), log);
await geo.load();

const sessions = new SessionCache();
const now = Math.floor(Date.now() / 1000);
// Oldest first, so the cache ends up in the order it keeps itself in.
for (const session of store.openSessions(now - SESSION_TIMEOUT).sort((a, b) => a.timestamp - b.timestamp)) {
  sessions.put(session);
}

let hosts = new Map<string, AnalyticsHost>();
let timezone = "UTC";

const stats = new Stats(new QueryEngine(store.db), log);

const ingest = new Ingest({
  store,
  sessions,
  salts: new Salts(store),
  userAgents: new UserAgents(),
  geo,
  // `d` arrives without `www.`, which a site configured under its `www` name
  // still has to be found by.
  host: (domain) => hosts.get(domain) ?? hosts.get(`www.${domain}`),
  flush,
});

const flushTimer = setInterval(flush, FLUSH_EVERY);
const sweepTimer = setInterval(() => sessions.sweep(Math.floor(Date.now() / 1000)), SWEEP_EVERY);

const server = Bun.serve({
  port: config.port,
  hostname: config.address,
  maxRequestBodySize: MAX_BODY,

  async fetch(request, bun) {
    const url = new URL(request.url);
    // Both spellings: a proxy may strip the prefix on the way in, or not.
    if (url.pathname !== "/api/event" && url.pathname !== EVENT_PATH) {
      return new Response("Not found.", { status: 404, headers: { "content-type": "text/plain; charset=utf-8" } });
    }

    const headers: SenderHeaders = {
      userAgent: request.headers.get("user-agent") ?? "",
      forwardedFor: request.headers.get("x-forwarded-for"),
      realIp: request.headers.get("x-real-ip"),
    };

    return toResponse(await receive(request, headers, bun.requestIP(request)?.address ?? ""));
  },

  error(error) {
    log.error("analytics_request_failed", errFields(error));
    return new Response("Something went wrong.", { status: 500, headers: CORS });
  },
});

/**
 * Answer one request for the event endpoint, however it arrived.
 *
 * The tracker posts `text/plain`, which is what spares it a preflight, but a
 * client of Plausible's API may send JSON and preflight it, so both are met.
 */
async function receive(request: Request, headers: SenderHeaders, socketIp: string): Promise<WorkerResponse> {
  if (request.method === "OPTIONS") return { status: 204, headers: {}, body: "" };
  if (request.method !== "POST") {
    return { status: 405, headers: { allow: "POST, OPTIONS" }, body: "Send events with POST." };
  }

  return ingest.handle(await request.text(), { ip: clientIp(headers, socketIp), userAgent: headers.userAgent });
}

/**
 * The visitor's address, used to tell visitors apart and to place them, and
 * then forgotten.
 *
 * The proxy's word for it, since behind one the socket is always the proxy.
 * Caddy replaces any `X-Forwarded-For` a client sends with the address it
 * actually saw, so the first entry is the client's; a proxy that appends
 * instead lets a visitor choose which address they are counted under, which
 * only miscounts that visitor.
 */
function clientIp(headers: SenderHeaders, socketIp: string): string {
  const forwarded = headers.forwardedFor?.split(",")[0]?.trim();
  return forwarded || headers.realIp?.trim() || socketIp;
}

function toResponse(result: WorkerResponse): Response {
  return new Response(result.body || null, { status: result.status, headers: { ...CORS, ...result.headers } });
}

function flush(): void {
  try {
    const written = store.flush();
    if (written.events || written.sessions) log.debug("analytics_flushed", written);
    if (store.dropped) {
      log.warn("analytics_buffer_full", { dropped: store.dropped });
      store.dropped = 0;
    }
  } catch (error) {
    log.error("analytics_flush_failed", errFields(error));
  }
}

/** Answer something the main thread asked. */
async function answer(request: WorkerRequest): Promise<WorkerResponse> {
  switch (request.type) {
    case "event":
      return await receive(
        new Request("http://analytics/api/event", {
          method: request.method,
          body: request.method === "POST" ? request.body : undefined,
        }),
        request.headers,
        "",
      );
    case "query":
      // Whatever arrived in the last few seconds is still in memory.
      flush();
      return stats.query(request.host, request.body, timezone);
    case "current-visitors":
      flush();
      return stats.currentVisitors(request.host);
    case "sparklines":
      flush();
      return stats.sparklines(request.hosts, timezone);
    case "suggestions":
      return stats.suggestions(request.host, request.filter, request.params, timezone);
  }
}

function send(message: FromWorker): void {
  port?.postMessage(message);
}

/** Write what's buffered, close the port, and end the thread. */
async function stop(code: number): Promise<never> {
  clearInterval(flushTimer);
  clearInterval(sweepTimer);
  geo.stop();
  flush();
  // Closing the port is what makes it free for the next thread: a thread that
  // exits holding a listening socket leaves it bound for the life of the
  // process.
  await server.stop(true);
  store.db.close();
  process.exit(code);
}

port.on("message", (message: ToWorker) => {
  switch (message.type) {
    case "hosts":
      hosts = new Map(message.hosts.map((host) => [host.hostname, host]));
      // Fetched only once something is counting: a gateway that never turns
      // analytics on never downloads a location database.
      if (config.downloadGeo && message.hosts.some((host) => host.counting)) geo.keepUpdated();
      break;

    case "settings":
      timezone = message.timezone;
      break;

    case "request":
      void answer(message.request).then((response) => send({ type: "reply", id: message.id, response }));
      break;

    case "stop":
      void stop(0);
      break;
  }
});

// Anything that escapes a handler ends this thread rather than leaving it in
// a state nobody reasoned about — cleanly, so its port can be bound again.
for (const event of ["uncaughtException", "unhandledRejection"] as const) {
  process.on(event, (error: unknown) => {
    log.error("analytics_thread_failed", errFields(error));
    void stop(1);
  });
}

log.info("analytics_started", { port: server.port, sessions: sessions.size });
send({ type: "ready", port: server.port });
