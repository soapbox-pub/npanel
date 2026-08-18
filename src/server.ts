import { dirname, join } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { createApp } from "./app.ts";
import { BlobStore } from "./blobs.ts";
import { BunkerHost } from "./bunker/host.ts";
import { Config } from "./config.ts";
import { openDatabase } from "./db/index.ts";
import { DnsTargets } from "./dns.ts";
import { Logger, errFields } from "./log.ts";
import { NostrService } from "./nostr.ts";
import { SettingsService } from "./settings.ts";
import { SyncWorker } from "./sync.ts";
import { startTlsCheckServer } from "./tls-check.ts";

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
const dns = new DnsTargets({ dashboardHost: config.dashboardHost, settings, log });

// The NIP-46 gateway runs in its own thread, so relay sockets and signing
// round trips can't slow down or take down the serving of sites. Without a
// SECRET_KEY there is nowhere safe to keep its credentials, and it stays off.
const bunkers = new BunkerHost({
  secretKey: config.secretKey,
  databasePath: config.databasePath,
  dashboardHost: config.dashboardHost,
  logLevel: config.logLevel,
  log,
});

await sync.start();
bunkers.start();

// The worker holds its own settings cache, read from the same database, so an
// edit made here has to be pushed across rather than waited for.
settings.onChange(() => {
  void bunkers.reload().catch(() => {});
});

// A second listener, on its own port, so the proxy can ask which hostnames may
// have a certificate without that question being answerable from the internet.
const tlsCheck = startTlsCheckServer(
  { db, settings, log, dashboardHost: config.dashboardHost },
  { port: config.tlsCheckPort, address: config.bindAddress },
);

const app = createApp({
  db,
  settings,
  sync,
  bunkers,
  blobs,
  dns,
  log,
  dashboardHost: config.dashboardHost,
  dashboardDir: join(dirname(fileURLToPath(import.meta.url)), "..", "dashboard", "dist"),
});

log.info("started", {
  bind_address: config.bindAddress,
  port: config.port,
  tls_check_port: config.tlsCheckPort,
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
    await tlsCheck.stop();
    await bunkers.stop();
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
