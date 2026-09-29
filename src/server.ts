import { dirname, join } from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";
import { AnalyticsService } from "./analytics/service.ts";
import { createApp } from "./app.ts";
import { BlobStore } from "./blobs.ts";
import { Config } from "./config.ts";
import { openDatabase } from "./db/index.ts";
import { DeployService } from "./deploy/service.ts";
import { DnsTargets } from "./dns.ts";
import { Logger, errFields } from "./log.ts";
import { ServeCache } from "./serve-cache.ts";
import { createSiteServer } from "./serve-site.ts";
import { SettingsService } from "./settings.ts";
import { SiteCsp } from "./site-csp.ts";
import { SitePreviews } from "./site-preview.ts";
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
// One instance: serving reads it, and every writer that changes what serving
// would answer — the hosts API, the deploy thread, Tor — clears it.
const serveCache = new ServeCache(db);
// Relays, downloads, compression and builds, on a thread of their own so that
// a deploy never makes a visitor wait.
const deployments = new DeployService({
  settings,
  serveCache,
  log,
  config: { databasePath: config.databasePath, blobsDir: config.blobsDir, logLevel: config.logLevel },
});
const dns = new DnsTargets({ dashboardHost: config.dashboardHost, settings, log });
// One instance, so the serving path and the dashboard share its cache.
const siteCsp = new SiteCsp(db, blobs, log);
const previews = new SitePreviews(blobs);
// npanel's Plausible server, on a thread and a port of its own so that nothing
// it does competes with serving sites.
const analytics = new AnalyticsService({
  db,
  settings,
  log,
  config: {
    port: config.analyticsPort,
    address: config.bindAddress,
    dataDir: config.dataDir,
    logLevel: config.logLevel,
    downloadGeo: true,
  },
});
const serveSite = createSiteServer({ db, blobs, siteCsp, serveCache, analytics, log });
const tor = new TorService({ db, settings, log, serveSite, serveCache, deployments });

// Neither thread is awaited: one starting late, dying and being restarted, or
// failing to bind its port and retrying, must never hold up serving sites.
// Whatever the API asks of the deploy thread meanwhile is held until it's up.
void deployments.start();
void analytics.start();

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
  deployments,
  analytics,
  blobs,
  dns,
  log,
  tor,
  siteCsp,
  previews,
  serveCache,
  serveSite,
  dashboardHost: config.dashboardHost,
  dashboardDir: join(dirname(fileURLToPath(import.meta.url)), "..", "dashboard", "dist"),
});

log.info("started", {
  bind_address: config.bindAddress,
  port: config.port,
  tls_check_port: config.tlsCheckPort,
  analytics_port: config.analyticsPort,
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
    await deployments.stop();
    await analytics.stop();
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
