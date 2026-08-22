import { dirname, join } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { createApp } from "./app.ts";
import { BlobStore } from "./blobs.ts";
import { Config } from "./config.ts";
import { openDatabase } from "./db/index.ts";
import { DnsTargets } from "./dns.ts";
import { Logger, errFields } from "./log.ts";
import { NostrService } from "./nostr.ts";
import { createSiteServer } from "./serve-site.ts";
import { SettingsService } from "./settings.ts";
import { SiteCsp } from "./site-csp.ts";
import { SyncWorker } from "./sync.ts";
import { startTlsCheckServer } from "./tls-check.ts";
import { TorService } from "./tor.ts";

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
// One instance, so the serving path and the dashboard share its cache.
const siteCsp = new SiteCsp(db, blobs, log);
const serveSite = createSiteServer({ db, blobs, siteCsp, log });
const tor = new TorService({ db, settings, log, serveSite });

await sync.start();

// Onion services, if an admin has turned them on. Everything here is driven by
// the setting, so a gateway with Tor off never opens a socket for it. Not
// awaited: an unreachable Tor retries on its own, and no other listener should
// wait for it.
void tor.start();

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
  blobs,
  dns,
  log,
  tor,
  siteCsp,
  serveSite,
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
    await tor.stop();
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
