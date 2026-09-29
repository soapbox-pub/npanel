import type { LogLevel } from "../log.ts";
import type { NsiteAddress } from "../nsite-address.ts";
import type { SettingsValues } from "../settings.ts";

/** What the deploy thread is started with. */
export interface DeployConfig {
  /** The main thread's database, already migrated. */
  databasePath: string;
  blobsDir: string;
  logLevel: LogLevel;
}

/** Messages the main thread sends the deploy thread. */
export type ToDeploy =
  /** Every setting, as the main thread has just written them. */
  | { type: "settings"; values: SettingsValues }
  | { type: "hosts-changed" }
  | { type: "rebuild"; hostIds: number[] }
  | { type: "sync-site"; address: NsiteAddress }
  | { type: "stop" };

/** Messages the deploy thread sends back. */
export type FromDeploy =
  | { type: "ready" }
  /** A build went live or was dropped, so serving's cached answers are stale. */
  | { type: "serving-changed" };
