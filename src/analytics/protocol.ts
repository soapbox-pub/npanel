import type { LogLevel } from "../log.ts";
import type { AnalyticsHost, IngestResponse } from "./ingest.ts";

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

/** Messages the main thread sends the analytics thread. */
export type ToWorker =
  | { type: "hosts"; hosts: AnalyticsHost[] }
  /** An event that reached the main thread instead: over Tor, or with no proxy route for it. */
  | { type: "event"; id: number; method: string; body: string; headers: SenderHeaders }
  | { type: "stop" };

/** Messages the analytics thread sends back. */
export type FromWorker = { type: "ready"; port: number } | { type: "event"; id: number; response: IngestResponse };
