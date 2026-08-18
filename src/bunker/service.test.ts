import assert from "node:assert/strict";
import { after, test } from "node:test";
import type { Kysely } from "kysely";
import { getPublicKey } from "nostr-tools";
import { openDatabase } from "../db/index.ts";
import type { Bunker, Database } from "../db/schema.ts";
import { Logger } from "../log.ts";
import { SettingsService } from "../settings.ts";
import { fakeNostr } from "../test/harness.ts";
import { type FakeSigner, fakeSigner, waitFor } from "../test/nip46.ts";
import { Secrets } from "./secrets.ts";
import { BunkerService } from "./service.ts";

const KEY = "11".repeat(32);
const RELAY = "wss://relay.test";

const started: BunkerService[] = [];
const databases: Kysely<Database>[] = [];

after(async () => {
  for (const service of started) await service.stop();
  for (const db of databases) await db.destroy();
});

async function setup(opts: { signer?: FakeSigner; secretKey?: string } = {}) {
  const signer = opts.signer ?? fakeSigner();

  const db = await openDatabase(":memory:");
  databases.push(db);

  const settings = await SettingsService.load(db);
  await settings.update({ bunker_relays: [RELAY] }, "system");

  // Both halves of the gateway's relay access reach the same fake signer: the
  // pairing listener through `req`, and every later request through `group`.
  const nostr = fakeNostr({
    req: (filters, reqOpts) => signer.relay.req(filters, reqOpts),
    group: () => signer.relay,
  });

  const service = new BunkerService({
    db,
    nostr,
    settings,
    secrets: Secrets.fromHex(opts.secretKey ?? KEY),
    log: new Logger("error"),
    dashboardHost: "panel.test",
  });
  service.start();
  started.push(service);

  const now = Math.floor(Date.now() / 1000);
  const { id } = await db
    .insertInto("bunkers")
    .values({
      slug: "press",
      name: "Press",
      user_pubkey: null,
      upstream_pubkey: null,
      upstream_secret: null,
      upstream_relays: "[]",
      key_fingerprint: null,
      status: "pending",
      error: null,
      last_ok_at: null,
      enabled: 1,
      created_by: "00".repeat(32),
      created_at: now,
      updated_at: now,
    })
    .returning("id")
    .executeTakeFirstOrThrow();

  const reload = async (): Promise<Bunker> =>
    await db.selectFrom("bunkers").selectAll().where("id", "=", id).executeTakeFirstOrThrow();

  return { db, service, signer, bunkerId: id, reload };
}

function invitation(uri: string): { clientPubkey: string; params: URLSearchParams } {
  const [authority, query] = uri.slice("nostrconnect://".length).split("?", 2);
  return { clientPubkey: authority, params: new URLSearchParams(query) };
}

test("invites a signer with everything it needs to answer", async () => {
  const { service, bunkerId } = await setup();

  const pairing = await service.startPairing({ bunkerId });

  assert.equal(pairing.state, "waiting");
  assert.ok(pairing.uri);
  assert.ok(pairing.uri.startsWith("nostrconnect://"));

  const { clientPubkey, params } = invitation(pairing.uri);

  assert.match(clientPubkey, /^[0-9a-f]{64}$/);
  assert.deepEqual(params.getAll("relay"), [RELAY]);
  assert.match(params.get("secret") ?? "", /^[0-9a-f]{16}$/);
  assert.equal(params.get("name"), "npanel");
  assert.equal(params.get("url"), "https://panel.test");
  assert.ok(params.get("perms")?.includes("sign_event"));
});

test("pairs when a signer accepts the invitation", async () => {
  const { service, signer, bunkerId, reload } = await setup();

  const pairing = await service.startPairing({ bunkerId });
  const { clientPubkey, params } = invitation(pairing.uri ?? "");

  signer.announce(clientPubkey, params.get("secret") ?? "");

  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "connected", "the pairing to settle");

  const bunker = await reload();
  assert.equal(bunker.status, "connected");
  assert.equal(bunker.user_pubkey, signer.userPubkey);
  assert.equal(bunker.upstream_pubkey, signer.pubkey);
  assert.deepEqual(JSON.parse(bunker.upstream_relays), [RELAY]);
  assert.ok(bunker.last_ok_at);

  // Learning who the bunker signs for is a `get_public_key` upstream, and the
  // one call is the whole cost of pairing.
  assert.deepEqual(
    signer.requests.map((request) => request.method),
    ["get_public_key"],
  );
});

test("ignores an event from someone who isn't the signer", async () => {
  const { service, signer, bunkerId } = await setup();

  const pairing = await service.startPairing({ bunkerId });
  const { clientPubkey } = invitation(pairing.uri ?? "");

  signer.announce(clientPubkey, "not-the-secret");
  await new Promise((resolve) => setTimeout(resolve, 50));

  assert.equal((await service.pairingStatus(bunkerId)).state, "waiting");
});

test("keeps the credential encrypted at rest", async () => {
  const { service, signer, bunkerId, reload } = await setup();

  const pairing = await service.startPairing({ bunkerId });
  const { clientPubkey, params } = invitation(pairing.uri ?? "");
  signer.announce(clientPubkey, params.get("secret") ?? "");

  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "connected", "the pairing to settle");

  const bunker = await reload();
  const sealed = bunker.upstream_secret ?? "";

  assert.ok(sealed.length);
  assert.ok(!/^[0-9a-f]{64}$/.test(sealed), "the stored credential is not a bare key");
  assert.equal(bunker.key_fingerprint, Secrets.fromHex(KEY).fingerprint);

  // It really is the client key the invitation was published under.
  const clientKey = Uint8Array.from(Buffer.from(Secrets.fromHex(KEY).decrypt(sealed), "hex"));
  assert.equal(getPublicKey(clientKey), clientPubkey);
});

test("consumes a bunker:// URI the signer produced", async () => {
  const { service, signer, bunkerId, reload } = await setup();

  const uri = `bunker://${signer.pubkey}?relay=${encodeURIComponent(RELAY)}&secret=hunter2`;
  await service.startPairing({ bunkerId, uri });

  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "connected", "the pairing to settle");

  const bunker = await reload();
  assert.equal(bunker.upstream_pubkey, signer.pubkey);
  assert.equal(bunker.user_pubkey, signer.userPubkey);

  const connect = signer.requests.find((request) => request.method === "connect");
  assert.equal(connect?.params[1], "hunter2");
});

test("refuses a bunker:// URI with nowhere to reach the signer", async () => {
  const { service, signer, bunkerId } = await setup();

  await service.startPairing({ bunkerId, uri: `bunker://${signer.pubkey}` });

  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "failed", "the pairing to fail");

  assert.match((await service.pairingStatus(bunkerId)).error ?? "", /no relays/);
});

test("says a bunker:// secret may already be spent when connect fails", async () => {
  const { service, signer, bunkerId } = await setup();

  signer.fail("connect", "invalid secret");
  await service.startPairing({ bunkerId, uri: `bunker://${signer.pubkey}?relay=${encodeURIComponent(RELAY)}` });

  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "failed", "the pairing to fail");

  assert.match((await service.pairingStatus(bunkerId)).error ?? "", /fresh URI/);
});

test("a second attempt supersedes the first", async () => {
  const { service, signer, bunkerId } = await setup();

  const first = await service.startPairing({ bunkerId });
  const second = await service.startPairing({ bunkerId });

  assert.notEqual(first.uri, second.uri);

  // The abandoned invitation is no longer listened for.
  signer.announce(invitation(first.uri ?? "").clientPubkey, invitation(first.uri ?? "").params.get("secret") ?? "");
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal((await service.pairingStatus(bunkerId)).state, "waiting");

  signer.announce(invitation(second.uri ?? "").clientPubkey, invitation(second.uri ?? "").params.get("secret") ?? "");
  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "connected", "the second to settle");
});

test("reports an unpaired bunker rather than pretending", async () => {
  const { service, bunkerId, reload } = await setup();

  const result = await service.testBunker(bunkerId);

  assert.equal(result.ok, false);
  assert.match(result.error ?? "", /isn't connected to a signer yet/);
  // Unpaired is not broken.
  assert.equal((await reload()).status, "pending");
});

test("checks a paired bunker by asking who it is", async () => {
  const { service, signer, bunkerId, reload } = await setup();

  const pairing = await service.startPairing({ bunkerId });
  const { clientPubkey, params } = invitation(pairing.uri ?? "");
  signer.announce(clientPubkey, params.get("secret") ?? "");
  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "connected", "the pairing to settle");

  const result = await service.testBunker(bunkerId);

  assert.equal(result.ok, true);
  assert.equal(result.user_pubkey, signer.userPubkey);
  assert.equal((await reload()).status, "connected");
});

test("records an upstream that stopped answering", async () => {
  const { service, signer, bunkerId, reload } = await setup();

  const pairing = await service.startPairing({ bunkerId });
  const { clientPubkey, params } = invitation(pairing.uri ?? "");
  signer.announce(clientPubkey, params.get("secret") ?? "");
  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "connected", "the pairing to settle");

  signer.fail("get_public_key", "the user rejected the request");
  const result = await service.testBunker(bunkerId);

  assert.equal(result.ok, false);
  const bunker = await reload();
  assert.equal(bunker.status, "error");
  assert.match(bunker.error ?? "", /rejected/);
});

test("refuses a credential sealed with a different SECRET_KEY", async () => {
  const signer = fakeSigner();
  const { service, bunkerId, db } = await setup({ signer });

  const pairing = await service.startPairing({ bunkerId });
  const { clientPubkey, params } = invitation(pairing.uri ?? "");
  signer.announce(clientPubkey, params.get("secret") ?? "");
  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "connected", "the pairing to settle");

  // What restarting with the wrong key looks like from the row's point of view.
  await db.updateTable("bunkers").set({ key_fingerprint: "deadbeefdeadbeef" }).where("id", "=", bunkerId).execute();

  const result = await service.testBunker(bunkerId);

  assert.equal(result.ok, false);
  assert.match(result.error ?? "", /different SECRET_KEY/);
});
