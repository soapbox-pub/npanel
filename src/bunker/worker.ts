import { parentPort, workerData } from "node:worker_threads";
import { openDatabase } from "../db/index.ts";
import { type LogLevel, Logger } from "../log.ts";
import { NostrService } from "../nostr.ts";
import { SettingsService } from "../settings.ts";
import type { BunkerCall } from "./api.ts";
import { Secrets } from "./secrets.ts";
import { BunkerService } from "./service.ts";

/**
 * The bunker gateway's thread.
 *
 * It exists so that relay sockets, signing round trips to somebody's phone,
 * and anything that goes wrong with either stay off the thread that serves
 * nsites. Serving sites is what npanel is for; bunkers are what it also does,
 * and the two should not be able to take each other down.
 *
 * The database is opened a second time here rather than shared. SQLite is in
 * WAL mode with a busy timeout (see {@link file://../db/sqlite.ts}), so two
 * connections are fine, and the main thread has already run the migrations by
 * the time it spawns this.
 */

export interface BunkerWorkerData {
  databasePath: string;
  secretKey: string;
  dashboardHost: string;
  logLevel: LogLevel;
}

type Reply = { id: number; ok: true; value: unknown } | { id: number; ok: false; error: string };

const port = parentPort;
if (!port) {
  throw new Error("The bunker worker must be started as a worker thread.");
}

const data = workerData as BunkerWorkerData;
const log = new Logger(data.logLevel);

const db = await openDatabase(data.databasePath, log);
const settings = await SettingsService.load(db, log);
const nostr = new NostrService(settings, log);
const service = new BunkerService({
  db,
  nostr,
  settings,
  secrets: Secrets.fromHex(data.secretKey),
  log,
  dashboardHost: data.dashboardHost,
});

await service.start();

port.on("message", (call: BunkerCall & { id: number }) => {
  void dispatch(call).then(
    (value) => port.postMessage({ id: call.id, ok: true, value } satisfies Reply),
    (error: unknown) => {
      port.postMessage({
        id: call.id,
        ok: false,
        error: error instanceof Error ? error.message : String(error),
      } satisfies Reply);
    },
  );
});

/** Route one call to the service, narrowing its arguments as it goes. */
async function dispatch(call: BunkerCall): Promise<unknown> {
  switch (call.method) {
    case "reload":
      return await service.reload();
    case "startPairing":
      return await service.startPairing(...call.args);
    case "pairingStatus":
      return await service.pairingStatus(...call.args);
    case "cancelPairing":
      return await service.cancelPairing(...call.args);
    case "testBunker":
      return await service.testBunker(...call.args);
    case "mintConnection":
      return await service.mintConnection(...call.args);
    case "stop":
      // Signals only reach the main thread, so shutting down cleanly has to be
      // something the host asks for.
      await service.stop();
      return await db.destroy();
  }
}
