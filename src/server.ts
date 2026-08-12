import { dirname, join } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { createApp } from "./app.ts";
import { BlobStore } from "./blobs.ts";
import { Config } from "./config.ts";
import { openDatabase } from "./db/index.ts";
import { Logger, errFields } from "./log.ts";
import { NostrService } from "./nostr.ts";
import { SettingsService } from "./settings.ts";
import { SyncWorker } from "./sync.ts";

const config = new Config({
  get(key) {
    return process.env[key];
  },
});

const log = new Logger(config.logLevel);

const db = await openDatabase(config.databasePath, log);
const settings = await SettingsService.load(db, log);
const blobs = new BlobStore(config.blobsDir);
const nostr = new NostrService(settings, log);
const sync = new SyncWorker({ db, blobs, nostr, settings, log });

await sync.start();

const app = createApp({
  db,
  settings,
  sync,
  blobs,
  log,
  dashboardHost: config.dashboardHost,
  dashboardDir: join(dirname(fileURLToPath(import.meta.url)), "..", "dashboard", "dist"),
});

log.info("started", {
  bind_address: config.bindAddress,
  port: config.port,
  dashboard_host: config.dashboardHost,
  data_dir: config.dataDir,
});

for (const signal of ["SIGINT", "SIGTERM"] as const) {
  process.on(signal, () => {
    log.info("shutting_down", { signal });
    void shutdown();
  });
}

async function shutdown(): Promise<void> {
  try {
    await sync.stop();
    await db.destroy();
  } catch (error) {
    log.error("shutdown_failed", errFields(error));
  }
  process.exit(0);
}

export default {
  port: config.port,
  hostname: config.bindAddress,
  fetch: app.fetch,
};
