import assert from "node:assert/strict";
import { after, test } from "node:test";
import type { NostrEvent } from "@nostrify/nostrify";
import { BunkerURI } from "@nostrify/nostrify";
import type { Kysely } from "kysely";
import { verifyEvent } from "nostr-tools";
import { openDatabase } from "../db/index.ts";
import type { BunkerConnection, Database } from "../db/schema.ts";
import { Logger } from "../log.ts";
import { SettingsService } from "../settings.ts";
import { fakeNostr } from "../test/harness.ts";
import { FakeClient, MemoryRelay, fakeSigner, waitFor } from "../test/nip46.ts";
import { Secrets } from "./secrets.ts";
import { BunkerService } from "./service.ts";
import { formatNostrConnectUri } from "./uri.ts";

const KEY = "55".repeat(32);
const RELAY = "wss://relay.test";

const started: BunkerService[] = [];
const databases: Kysely<Database>[] = [];

after(async () => {
  for (const service of started) await service.stop();
  for (const db of databases) await db.destroy();
});

/**
 * A gateway with a bunker already paired to a signer.
 *
 * Two relay worlds, which is the shape of the thing being tested: `downstream`
 * carries clients talking to npanel, and the fake signer's own relay carries
 * npanel talking to the phone.
 */
async function setup(settings: Partial<{ bunker_rate_limit: number; bunker_max_relays: number }> = {}) {
  const signer = fakeSigner();
  const downstream = new MemoryRelay();

  const db = await openDatabase(":memory:");
  databases.push(db);

  const config = await SettingsService.load(db);
  await config.update({ bunker_relays: [RELAY], ...settings }, "system");

  const nostr = fakeNostr({
    req: (filters, opts) => downstream.req(filters, opts),
    publish: async (event: NostrEvent) => {
      await downstream.event(event);
    },
    group: () => signer.relay,
  });

  const service = new BunkerService({
    db,
    nostr,
    settings: config,
    secrets: Secrets.fromHex(KEY),
    log: new Logger("error"),
    dashboardHost: "panel.test",
  });
  await service.start();
  started.push(service);

  const now = Math.floor(Date.now() / 1000);
  const { id: bunkerId } = await db
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

  await service.startPairing({ bunkerId, uri: `bunker://${signer.pubkey}?relay=${encodeURIComponent(RELAY)}` });
  await waitFor(async () => (await service.pairingStatus(bunkerId)).state === "connected", "the bunker to pair");
  signer.requests.length = 0;

  const connectionOf = async (signerPubkey: string): Promise<BunkerConnection> =>
    await db
      .selectFrom("bunker_connections")
      .selectAll()
      .where("signer_pubkey", "=", signerPubkey)
      .executeTakeFirstOrThrow();

  return { db, service, signer, downstream, bunkerId, connectionOf };
}

/** Mint a `bunker://` connection and return its parts. */
async function mint(
  service: BunkerService,
  bunkerId: number,
  permissions: string[] = ["sign_event:1"],
  name = "CI",
): Promise<{ signerPubkey: string; secret: string }> {
  const minted = await service.mintConnection({ bunkerId, name, permissions, createdBy: "ab".repeat(32) });
  assert.ok(minted.uri);

  const uri = new BunkerURI(minted.uri);

  return { signerPubkey: minted.signer_pubkey, secret: uri.secret ?? "" };
}

test("mints a bunker URI a client can act on", async () => {
  const { service, bunkerId } = await setup();

  const minted = await service.mintConnection({
    bunkerId,
    name: "CI",
    permissions: ["sign_event:1"],
    createdBy: "ab".repeat(32),
  });

  assert.ok(minted.uri);
  const uri = new BunkerURI(minted.uri);

  assert.equal(uri.pubkey, minted.signer_pubkey);
  assert.deepEqual(uri.relays, [RELAY]);
  assert.match(uri.secret ?? "", /^[0-9a-f]{16}$/);
});

test("pairs a client that presents the secret, once", async () => {
  const { service, downstream, bunkerId, connectionOf } = await setup();
  const { signerPubkey, secret } = await mint(service, bunkerId);

  const client = new FakeClient(downstream);
  const connected = await client.call(signerPubkey, "connect", [signerPubkey, secret]);
  assert.equal(connected.result, "ack");

  const row = await connectionOf(signerPubkey);
  assert.equal(row.client_pubkey, client.pubkey);
  // Spent, but the connection itself keeps working.
  assert.equal(row.secret, null);

  // The same client reconnecting is not a second pairing.
  const again = await client.call(signerPubkey, "connect", [signerPubkey, secret]);
  assert.equal(again.result, "ack");

  // A replayed URI must not pair somebody else.
  const impostor = new FakeClient(downstream);
  const stolen = await impostor.call(signerPubkey, "connect", [signerPubkey, secret]);
  assert.match(stolen.error ?? "", /already in use/);
});

test("refuses a client with the wrong secret, or none", async () => {
  const { service, downstream, bunkerId } = await setup();
  const { signerPubkey } = await mint(service, bunkerId);

  const client = new FakeClient(downstream);

  const wrong = await client.call(signerPubkey, "connect", [signerPubkey, "not-it"]);
  assert.match(wrong.error ?? "", /isn't valid/);

  const none = await client.call(signerPubkey, "connect", [signerPubkey]);
  assert.match(none.error ?? "", /needs the secret/);
});

test("answers rather than ignoring a client that never connected", async () => {
  const { service, downstream, bunkerId } = await setup();
  const { signerPubkey } = await mint(service, bunkerId);

  const response = await new FakeClient(downstream).call(signerPubkey, "get_public_key");

  assert.match(response.error ?? "", /Send a connect request first/);
});

async function paired(setupResult: Awaited<ReturnType<typeof setup>>, permissions: string[] = ["sign_event:1"]) {
  const { signerPubkey, secret } = await mint(setupResult.service, setupResult.bunkerId, permissions);
  const client = new FakeClient(setupResult.downstream);
  await client.call(signerPubkey, "connect", [signerPubkey, secret]);

  return { client, signerPubkey };
}

test("answers get_public_key without troubling the signer", async () => {
  const context = await setup();
  const { client, signerPubkey } = await paired(context);

  const response = await client.call(signerPubkey, "get_public_key");

  assert.equal(response.result, context.signer.userPubkey);
  // Cached at pairing; a phone should not wake up for this.
  assert.deepEqual(context.signer.requests, []);
});

test("forwards a signature the connection is allowed to ask for", async () => {
  const context = await setup();
  const { client, signerPubkey } = await paired(context, ["sign_event:1"]);

  const template = JSON.stringify({ kind: 1, content: "hello", tags: [], created_at: 0 });
  const response = await client.call(signerPubkey, "sign_event", [template]);

  assert.equal(response.error, undefined);
  const signed = JSON.parse(response.result) as NostrEvent;

  assert.equal(signed.kind, 1);
  assert.equal(signed.content, "hello");
  // Signed by the identity the bunker stands for, not by anything npanel holds.
  assert.equal(signed.pubkey, context.signer.userPubkey);
  assert.ok(verifyEvent(signed));
});

test("refuses a kind nobody granted, and says which", async () => {
  const context = await setup();
  const { client, signerPubkey } = await paired(context, ["sign_event:1"]);

  const response = await client.call(signerPubkey, "sign_event", [JSON.stringify({ kind: 4, content: "", tags: [] })]);

  assert.match(response.error ?? "", /Not allowed: sign_event:4/);
  // Refused here, so the signer is never asked.
  assert.deepEqual(context.signer.requests, []);

  const row = await context.connectionOf(signerPubkey);
  assert.equal(row.last_error, "Refused sign_event:4");
});

test("refuses an event with no kind, which could not be checked", async () => {
  const context = await setup();
  const { client, signerPubkey } = await paired(context, ["sign_event"]);

  const response = await client.call(signerPubkey, "sign_event", [JSON.stringify({ content: "hi" })]);

  assert.match(response.error ?? "", /no kind/);
  assert.deepEqual(context.signer.requests, []);
});

test("forwards encryption a connection was granted, and refuses the rest", async () => {
  const context = await setup();
  const { client, signerPubkey } = await paired(context, ["nip44_decrypt"]);

  const allowed = await client.call(signerPubkey, "nip44_decrypt", ["ff".repeat(32), "ciphertext"]);
  assert.equal(context.signer.requests.at(-1)?.method, "nip44_decrypt");
  assert.ok(allowed.error === undefined || !allowed.error.startsWith("Not allowed"));

  const refused = await client.call(signerPubkey, "nip44_encrypt", ["ff".repeat(32), "plaintext"]);
  assert.match(refused.error ?? "", /Not allowed: nip44_encrypt/);
});

test("rejects a method it doesn't implement", async () => {
  const context = await setup();
  const { client, signerPubkey } = await paired(context);

  const response = await client.call(signerPubkey, "sign_psbt", ["deadbeef"]);

  assert.match(response.error ?? "", /Unsupported method: sign_psbt/);
});

test("answers in whichever encryption it was asked", async () => {
  const context = await setup();
  const { signerPubkey, secret } = await mint(context.service, context.bunkerId);

  const client = new FakeClient(context.downstream);
  const response = await client.call(signerPubkey, "connect", [signerPubkey, secret], "nip04");

  assert.equal(response.result, "ack");

  const answer = context.downstream.published.find(
    (event) => event.pubkey === signerPubkey && event.tags.some((tag) => tag[1] === client.pubkey),
  );
  assert.ok(answer?.content.includes("?iv="), "answered a NIP-04 request with NIP-04");
});

test("ends a session on request", async () => {
  const context = await setup();
  const { client, signerPubkey } = await paired(context);

  const response = await client.call(signerPubkey, "logout");
  assert.equal(response.result, "ack");

  const rows = await context.db.selectFrom("bunker_connections").selectAll().execute();
  assert.deepEqual(rows, []);
});

test("moves a client onto the gateway's own relays", async () => {
  const context = await setup();
  const invitation = new FakeClient(context.downstream);

  // A client that named a relay of its own is the case worth moving.
  const uri = formatNostrConnectUri({
    clientPubkey: invitation.pubkey,
    relays: ["wss://elsewhere.test"],
    secret: "s3cret",
  });
  const minted = await context.service.mintConnection({
    bunkerId: context.bunkerId,
    name: "Roaming",
    permissions: [],
    createdBy: "ab".repeat(32),
    uri,
  });

  const response = await invitation.call(minted.signer_pubkey, "switch_relays");

  assert.deepEqual(JSON.parse(response.result), [RELAY]);
  assert.deepEqual(JSON.parse((await context.connectionOf(minted.signer_pubkey)).relays), [RELAY]);

  // Nothing to change the second time.
  const again = await invitation.call(minted.signer_pubkey, "switch_relays");
  assert.equal(again.result, "null");
});

test("answers a client's own invitation with its secret", async () => {
  const context = await setup();
  const client = new FakeClient(context.downstream);

  const uri = formatNostrConnectUri({
    clientPubkey: client.pubkey,
    relays: [RELAY],
    secret: "prove-it",
    perms: ["sign_event:1"],
    name: "Some App",
  });

  const minted = await context.service.mintConnection({
    bunkerId: context.bunkerId,
    name: "Some App",
    permissions: ["sign_event:1"],
    createdBy: "ab".repeat(32),
    uri,
  });

  assert.equal(minted.uri, null);

  // The client is bound already, so it can work without connecting first.
  const row = await context.connectionOf(minted.signer_pubkey);
  assert.equal(row.client_pubkey, client.pubkey);
  assert.equal(row.secret, null);

  const answer = context.downstream.published.find((event) => event.pubkey === minted.signer_pubkey);
  assert.ok(answer, "npanel answered the invitation");

  const response = await client.call(minted.signer_pubkey, "get_public_key");
  assert.equal(response.result, context.signer.userPubkey);
});

test("refuses an invitation that carries no secret or no relay", async () => {
  const context = await setup();

  for (const uri of [
    `nostrconnect://${"aa".repeat(32)}?relay=${encodeURIComponent(RELAY)}`,
    `nostrconnect://${"aa".repeat(32)}?secret=x`,
    "nostrconnect://not-a-pubkey?relay=wss://relay.test&secret=x",
  ]) {
    await assert.rejects(() =>
      context.service.mintConnection({
        bunkerId: context.bunkerId,
        name: "Bad",
        permissions: [],
        createdBy: "ab".repeat(32),
        uri,
      }),
    );
  }
});

test("won't hold more relays than it was allowed", async () => {
  const context = await setup({ bunker_max_relays: 1 });

  // The gateway's own relay is already one; the client's would be a second.
  const uri = formatNostrConnectUri({
    clientPubkey: "aa".repeat(32),
    relays: ["wss://elsewhere.test"],
    secret: "x",
  });

  await mint(context.service, context.bunkerId);

  await assert.rejects(
    () =>
      context.service.mintConnection({
        bunkerId: context.bunkerId,
        name: "Roaming",
        permissions: [],
        createdBy: "ab".repeat(32),
        uri,
      }),
    /limited to 1/,
  );
});

test("refuses a client that will not slow down", async () => {
  const context = await setup({ bunker_rate_limit: 1 });
  const { client, signerPubkey } = await paired(context);

  // Pairing spent the connection's one token.
  const response = await client.call(signerPubkey, "ping");

  assert.match(response.error ?? "", /Too many requests/);
});

test("handles a duplicated delivery once", async () => {
  const context = await setup();
  const { signerPubkey, secret } = await mint(context.service, context.bunkerId);

  const client = new FakeClient(context.downstream);
  const request = client.request(signerPubkey, "connect", [signerPubkey, secret]);

  await context.downstream.event(request);
  await context.downstream.event(request);

  const answers = () => context.downstream.published.filter((event) => event.pubkey === signerPubkey);
  await waitFor(() => answers().length > 0, "an answer");
  await context.service.whenIdle();

  // Long enough that a second pass would have answered by now.
  await new Promise((resolve) => setTimeout(resolve, 50));

  assert.equal(answers().length, 1);
});

test("refuses everything through a bunker that was switched off", async () => {
  const context = await setup();
  const { client, signerPubkey } = await paired(context);

  await context.db.updateTable("bunkers").set({ enabled: 0 }).where("id", "=", context.bunkerId).execute();

  const response = await client.call(signerPubkey, "ping");

  assert.match(response.error ?? "", /switched off/);
});

test("keeps a connection's key encrypted at rest", async () => {
  const context = await setup();
  const { signerPubkey } = await mint(context.service, context.bunkerId);

  const row = await context.connectionOf(signerPubkey);

  assert.ok(row.signer_secret.length);
  assert.ok(!/^[0-9a-f]{64}$/.test(row.signer_secret));
  assert.equal(Secrets.fromHex(KEY).decrypt(row.signer_secret).length, 64);
});
