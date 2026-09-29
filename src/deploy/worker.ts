import process from "node:process";
import { parentPort, workerData } from "node:worker_threads";
import { BlobStore } from "../blobs.ts";
import { connectDatabase } from "../db/index.ts";
import { Logger, errFields } from "../log.ts";
import { NostrService } from "../nostr.ts";
import { SettingsService } from "../settings.ts";
import { Deployer } from "./deployer.ts";
import type { DeployConfig, FromDeploy, ToDeploy } from "./protocol.ts";

/**
 * The deploy thread: everything between a relay and a finished build.
 *
 * The relay pool, and with it every signature check on every event a relay
 * sends; the downloads and the hashing of every byte of them; compression at
 * brotli's slowest setting; and the rewriting and storing of every file a
 * build generates. None of it is on the thread that serves sites, which is
 * the point: a deploy of a large site used to be exactly the kind of work
 * that makes every visitor's request wait.
 *
 * It opens its own connection to the main thread's database, and writes what
 * it does there — versions, deploy logs, builds — where the main thread reads
 * it, so the only thing that has to be said across the channel is that a
 * build went live. What the main thread tells it is as small: settings, and
 * that a host or a site wants looking at again.
 *
 * An uncaught error ends the thread, and the main thread starts another. That
 * is safe because nothing here is tracked, only reconciled: a new thread
 * resyncs every site and rebuilds every host whose build is out of date, and
 * a deploy cut off halfway resumes from the blobs already on disk.
 */

const config = workerData as DeployConfig;
const port = parentPort;
if (!port) throw new Error("src/deploy/worker.ts runs as a worker thread, started by DeployService.");

const log = new Logger(config.logLevel);
const db = await connectDatabase(config.databasePath);
const settings = await SettingsService.load(db, log);

const deployer = new Deployer({
  db,
  blobs: new BlobStore(config.blobsDir),
  nostr: new NostrService(settings, log),
  settings,
  log,
  servingChanged: () => send({ type: "serving-changed" }),
});

// Anything that escapes a handler ends this thread rather than leaving it in
// a state nobody reasoned about.
for (const event of ["uncaughtException", "unhandledRejection"] as const) {
  process.on(event, (error: unknown) => {
    log.error("deploy_thread_failed", errFields(error));
    void stop(1);
  });
}

const started = deployer.start();
let stopping: Promise<never> | undefined;

function send(message: FromDeploy): void {
  port?.postMessage(message);
}

/**
 * Let what is in progress settle, close the database, and end the thread.
 *
 * A deploy cut short here is left `pending` rather than failed, so the next
 * thread picks it up.
 */
function stop(code: number): Promise<never> {
  stopping ??= (async () => {
    try {
      await started.catch(() => {});
      await deployer.stop();
      await db.destroy();
    } catch (error) {
      log.error("deploy_thread_stop_failed", errFields(error));
    }
    process.exit(code);
  })();

  return stopping;
}

port.on("message", (message: ToDeploy) => {
  switch (message.type) {
    case "settings":
      settings.adopt(message.values);
      break;

    case "hosts-changed":
      deployer.hostsChanged();
      break;

    case "rebuild":
      deployer.rebuild(message.hostIds);
      break;

    case "sync-site":
      deployer.syncSite(message.address);
      break;

    case "stop":
      void stop(0);
      break;
  }
});

await started;

log.info("deploy_thread_started");
send({ type: "ready" });
