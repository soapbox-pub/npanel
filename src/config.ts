import { resolve } from "node:path";
import type { LogLevel } from "./log.ts";

const LOG_LEVELS: LogLevel[] = ["debug", "info", "warn", "error"];

/**
 * Type-safe configuration backed by an injectable environment provider.
 *
 * npanel keeps file-based configuration deliberately tiny: only values that
 * are system-level, or that could lock an admin out of the dashboard if set
 * wrong, live here. Relays, Blossom fallbacks, file size limits and the resync
 * interval are stored in the database and edited from the dashboard — see
 * {@link file://./settings.ts}.
 */
export class Config {
  /** Environment provider used to look up configuration values. */
  private env: { get(key: string): string | undefined };

  constructor(env: { get(key: string): string | undefined }) {
    this.env = env;
  }

  /** Port the HTTP server listens on. */
  get port(): number {
    const value = this.env.get("PORT");
    if (!value) {
      return 3000;
    }
    const port = Number.parseInt(value, 10);
    if (Number.isNaN(port) || port < 1 || port > 65535) {
      throw new Error("PORT must be a valid port number (1-65535).");
    }
    return port;
  }

  /**
   * Hostname serving the dashboard and JSON API. Requests with any other
   * `Host` header are served as nsites.
   */
  get dashboardHost(): string {
    const value = this.env.get("DASHBOARD_HOST");
    if (!value) {
      throw new Error("DASHBOARD_HOST must be set, eg panel.example.com.");
    }
    const hostname = value.trim().toLowerCase();
    if (!hostname || /[/\s]/.test(hostname)) {
      throw new Error("DASHBOARD_HOST must be a bare hostname, without a scheme or path.");
    }
    return hostname.replace(/:\d+$/, "");
  }

  /** Minimum severity of emitted log entries. */
  get logLevel(): LogLevel {
    const value = this.env.get("LOG_LEVEL");
    if (!value) {
      return "info";
    }
    const level = value.toLowerCase() as LogLevel;
    if (!LOG_LEVELS.includes(level)) {
      throw new Error(`LOG_LEVEL must be one of: ${LOG_LEVELS.join(", ")}.`);
    }
    return level;
  }

  /** Directory holding the database and the blob store. */
  get dataDir(): string {
    return resolve(this.env.get("DATA_DIR") || "./data");
  }

  /** Path of the SQLite database file. */
  get databasePath(): string {
    return resolve(this.dataDir, "npanel.sqlite3");
  }

  /** Root of the content-addressed blob store. */
  get blobsDir(): string {
    return resolve(this.dataDir, "blobs");
  }
}
