import assert from "node:assert/strict";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { openDatabase } from "../db/index.ts";
import { Logger } from "../log.ts";
import { BUNKER_UNAVAILABLE, BunkerHost } from "./host.ts";

/**
 * The thread boundary itself.
 *
 * Everything the gateway does is tested against {@link BunkerService} directly,
 * where there is no worker; this is here because spawning one is the part that
 * can break without any of that noticing — a module specifier that doesn't
 * resolve, a payload that won't structured-clone.
 */

const cleanups: (() => Promise<void>)[] = [];

after(async () => {
  for (const cleanup of cleanups) await cleanup();
});

async function setup(secretKey: string | undefined) {
  const dir = await mkdtemp(join(tmpdir(), "npanel-bunker-"));
  const databasePath = join(dir, "npanel.sqlite3");

  // The main thread migrates before it ever spawns the worker.
  const db = await openDatabase(databasePath);
  await db.destroy();

  const host = new BunkerHost({
    secretKey,
    databasePath,
    dashboardHost: "panel.test",
    logLevel: "error",
    log: new Logger("error"),
  });

  cleanups.push(async () => {
    await host.stop();
    await rm(dir, { recursive: true, force: true });
  });

  return { host, databasePath };
}

test("stays switched off without a SECRET_KEY", async () => {
  const { host } = await setup(undefined);
  host.start();

  assert.equal(host.available, false);
  await assert.rejects(() => host.reload(), new RegExp(BUNKER_UNAVAILABLE.slice(0, 30)));
});

test("answers across the thread boundary", async () => {
  const { host } = await setup("22".repeat(32));
  host.start();

  assert.equal(host.available, true);

  // Reaches the worker, which reads settings from its own connection.
  await host.reload();

  assert.deepEqual(await host.pairingStatus(1), { state: "idle", uri: null, error: null });
});

test("carries an error back as an error", async () => {
  const { host } = await setup("33".repeat(32));
  host.start();

  await assert.rejects(() => host.startPairing({ bunkerId: 999 }), /Bunker not found/);
});

test("stops cleanly, and refuses work afterwards", async () => {
  const { host } = await setup("44".repeat(32));
  host.start();
  await host.reload();

  await host.stop();

  await assert.rejects(() => host.reload(), /isn't running/);
});
