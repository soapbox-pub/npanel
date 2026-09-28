import type { LogLevel } from "../log.ts";
import type { AnalyticsHost } from "./ingest.ts";
import type { QueryHost } from "./query/engine.ts";

/** What the analytics thread is started with. */
export interface WorkerConfig {
  port: number;
  address: string;
  /** Where `analytics.sqlite3` and the location database live. */
  dataDir: string;
  logLevel: LogLevel;
  /**
   * Whether the location database may be downloaded. Off only in tests, which
   * must never fetch sixty megabytes from the internet.
   */
  downloadGeo: boolean;
}

/** The request headers an event's sender is worked out from. */
export interface SenderHeaders {
  userAgent: string;
  forwardedFor: string | null;
  realIp: string | null;
}

/** Something the main thread asks the analytics thread, and waits for an answer to. */
export type WorkerRequest =
  /** An event that reached the main thread instead: over Tor, or with no proxy route for it. */
  | { type: "event"; method: string; body: string; headers: SenderHeaders }
  | { type: "query"; host: QueryHost; body: unknown }
  | { type: "current-visitors"; host: QueryHost }
  /** `params` are the query string of Plausible's suggestions endpoint. */
  | { type: "suggestions"; host: QueryHost; filter: string; params: Record<string, string> };

/** An answer, as an HTTP response the main thread passes on as it is. */
export interface WorkerResponse {
  status: number;
  headers: Record<string, string>;
  body: string;
}

/** Messages the main thread sends the analytics thread. */
export type ToWorker =
  | { type: "hosts"; hosts: AnalyticsHost[] }
  | { type: "settings"; timezone: string }
  | { type: "request"; id: number; request: WorkerRequest }
  | { type: "stop" };

/** Messages the analytics thread sends back. */
export type FromWorker = { type: "ready"; port: number } | { type: "reply"; id: number; response: WorkerResponse };
