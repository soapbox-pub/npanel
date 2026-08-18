import assert from "node:assert/strict";
import { after, test } from "node:test";
import type { BunkerGateway, PairingStatus } from "../bunker/api.ts";
import { type Harness, createHarness, fakeBunkers, testUser } from "../test/harness.ts";

const harnesses: Harness[] = [];

after(async () => {
  for (const harness of harnesses) await harness.cleanup();
});

async function setup(bunkers: BunkerGateway = fakeBunkers({ available: true })) {
  const harness = await createHarness({ bunkers });
  harnesses.push(harness);

  const admin = testUser();
  const staff = testUser();
  const other = testUser();

  await harness.addUser(admin, "admin");
  await harness.addUser(staff, "staff");
  await harness.addUser(other, "staff");

  return { harness, admin, staff, other };
}

test("says so when the server has no SECRET_KEY", async () => {
  const { harness, admin } = await setup(fakeBunkers());

  const response = await harness.request(admin, "GET", "/api/bunkers");

  assert.equal(response.status, 503);
  assert.match((await response.json()).error, /SECRET_KEY/);
});

test("creates a bunker with a slug made from its name", async () => {
  const { harness, admin } = await setup();

  const response = await harness.request(admin, "POST", "/api/bunkers", { name: "Press Team!" });
  assert.equal(response.status, 201);

  const { bunker } = await response.json();
  assert.equal(bunker.slug, "press-team");
  assert.equal(bunker.name, "Press Team!");
  assert.equal(bunker.status, "pending");
  assert.equal(bunker.user_pubkey, null);
  assert.equal(bunker.enabled, true);
});

test("never serializes the stored credential", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/bunkers", { name: "Press" });
  await harness.db
    .updateTable("bunkers")
    .set({ upstream_secret: "sealed-credential", key_fingerprint: "abcdef", upstream_pubkey: "ff".repeat(32) })
    .where("slug", "=", "press")
    .execute();

  const body = await (await harness.request(admin, "GET", "/api/bunkers/press")).text();

  assert.ok(!body.includes("sealed-credential"));
  assert.ok(!body.includes("abcdef"));
  // The upstream's pubkey is half of a bunker URI, and is not a secret.
  assert.ok(body.includes("ff".repeat(32)));
});

test("refuses a name with no slug in it, and a slug already taken", async () => {
  const { harness, admin } = await setup();

  const empty = await harness.request(admin, "POST", "/api/bunkers", { name: "   " });
  assert.equal(empty.status, 400);

  const symbols = await harness.request(admin, "POST", "/api/bunkers", { name: "!!!" });
  assert.equal(symbols.status, 400);
  assert.match((await symbols.json()).error, /letters or digits/);

  await harness.request(admin, "POST", "/api/bunkers", { name: "Press" });
  const clash = await harness.request(admin, "POST", "/api/bunkers", { name: "press" });
  assert.equal(clash.status, 409);
});

test("staff see only the bunkers they were added to", async () => {
  const { harness, admin, staff, other } = await setup();

  await harness.request(admin, "POST", "/api/bunkers", { name: "Alpha" });
  await harness.request(admin, "PUT", "/api/bunkers/alpha/grants", { pubkeys: [staff.pubkey] });
  await harness.request(admin, "POST", "/api/bunkers", { name: "Beta" });

  const mine = await (await harness.request(staff, "GET", "/api/bunkers")).json();
  assert.deepEqual(
    mine.bunkers.map((bunker: { slug: string }) => bunker.slug),
    ["alpha"],
  );

  const forbidden = await harness.request(staff, "GET", "/api/bunkers/beta");
  assert.equal(forbidden.status, 403);
  assert.match((await forbidden.json()).error, /don't have access/);

  const none = await (await harness.request(other, "GET", "/api/bunkers")).json();
  assert.deepEqual(none.bunkers, []);

  const all = await (await harness.request(admin, "GET", "/api/bunkers")).json();
  assert.equal(all.bunkers.length, 2);
});

test("staff who create a bunker can manage it", async () => {
  const { harness, staff, other } = await setup();

  const created = await harness.request(staff, "POST", "/api/bunkers", { name: "Mine" });
  assert.equal(created.status, 201);

  assert.equal((await harness.request(staff, "GET", "/api/bunkers/mine")).status, 200);
  assert.equal((await harness.request(other, "GET", "/api/bunkers/mine")).status, 403);
});

test("only admins manage grants", async () => {
  const { harness, admin, staff } = await setup();

  await harness.request(admin, "POST", "/api/bunkers", { name: "Alpha" });
  await harness.request(admin, "PUT", "/api/bunkers/alpha/grants", { pubkeys: [staff.pubkey] });

  const denied = await harness.request(staff, "GET", "/api/bunkers/alpha/grants");
  assert.equal(denied.status, 403);

  const { grants } = await (await harness.request(admin, "GET", "/api/bunkers/alpha/grants")).json();
  assert.deepEqual(
    grants.map((grant: { user_pubkey: string }) => grant.user_pubkey),
    [staff.pubkey],
  );

  const stranger = testUser();
  const unknown = await harness.request(admin, "PUT", "/api/bunkers/alpha/grants", { pubkeys: [stranger.pubkey] });
  assert.equal(unknown.status, 400);
  assert.match((await unknown.json()).error, /Not npanel users/);
});

test("renames and disables without disturbing the stored signer", async () => {
  const { harness, admin } = await setup();

  const created = await (await harness.request(admin, "POST", "/api/bunkers", { name: "Press" })).json();

  const patched = await harness.request(admin, "PATCH", "/api/bunkers/press", { name: "Newsroom", enabled: false });
  const { bunker } = await patched.json();

  assert.equal(bunker.name, "Newsroom");
  assert.equal(bunker.enabled, false);
  // The slug is the URL, so a rename must not move the bunker.
  assert.equal(bunker.slug, "press");
  // The gateway caches signers by this, and a rename is not a re-pair.
  assert.equal(bunker.updated_at, created.bunker.updated_at);
});

test("deleting a bunker takes its grants with it", async () => {
  const { harness, admin, staff } = await setup();

  await harness.request(admin, "POST", "/api/bunkers", { name: "Alpha" });
  await harness.request(admin, "PUT", "/api/bunkers/alpha/grants", { pubkeys: [staff.pubkey] });

  assert.equal((await harness.request(admin, "DELETE", "/api/bunkers/alpha")).status, 200);
  assert.equal((await harness.request(admin, "GET", "/api/bunkers/alpha")).status, 404);
  assert.deepEqual(await harness.db.selectFrom("bunker_grants").selectAll().execute(), []);
});

test("pairing hands back the invitation to show", async () => {
  const waiting: PairingStatus = {
    state: "waiting",
    uri: "nostrconnect://abcd?relay=wss%3A%2F%2Frelay.test&secret=deadbeef",
    error: null,
  };
  const calls: { bunkerId: number; uri?: string }[] = [];

  const { harness, admin } = await setup(
    fakeBunkers({
      available: true,
      startPairing: async (input) => {
        calls.push(input);
        return waiting;
      },
      pairingStatus: async () => waiting,
    }),
  );

  await harness.request(admin, "POST", "/api/bunkers", { name: "Press" });

  const started = await harness.request(admin, "POST", "/api/bunkers/press/pair", {});
  assert.equal(started.status, 202);
  assert.deepEqual((await started.json()).pairing, waiting);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].uri, undefined);

  const polled = await harness.request(admin, "GET", "/api/bunkers/press/pair");
  assert.deepEqual((await polled.json()).pairing, waiting);

  await harness.request(admin, "POST", "/api/bunkers/press/pair", { uri: "bunker://ff?relay=wss://relay.test" });
  assert.equal(calls[1].uri, "bunker://ff?relay=wss://relay.test");
});

test("a gateway that is down fails the route, not the request path", async () => {
  const { harness, admin } = await setup(
    fakeBunkers({
      available: true,
      testBunker: async () => {
        throw new Error("The bunker gateway restarted. Try again.");
      },
    }),
  );

  await harness.request(admin, "POST", "/api/bunkers", { name: "Press" });

  const response = await harness.request(admin, "POST", "/api/bunkers/press/test");
  assert.equal(response.status, 502);
  assert.match((await response.json()).error, /restarted/);

  // Everything else still works.
  assert.equal((await harness.request(admin, "GET", "/api/bunkers/press")).status, 200);
});

test("mints a connection and shows its URI exactly once", async () => {
  const minted = { signer_pubkey: "cc".repeat(32), uri: "bunker://cc?relay=wss%3A%2F%2Frelay.test&secret=abcd" };
  const calls: { name: string; permissions: string[]; createdBy: string }[] = [];

  const { harness, admin } = await setup(
    fakeBunkers({
      available: true,
      mintConnection: async (input) => {
        calls.push(input);
        await harness.db
          .insertInto("bunker_connections")
          .values({
            bunker_id: 1,
            name: input.name,
            signer_secret: "sealed",
            signer_pubkey: minted.signer_pubkey,
            client_pubkey: null,
            secret: "abcd",
            relays: JSON.stringify(["wss://relay.test"]),
            permissions: JSON.stringify(input.permissions),
            created_by: input.createdBy,
            created_at: Math.floor(Date.now() / 1000),
            last_used_at: null,
            last_error: null,
          })
          .execute();
        return minted;
      },
    }),
  );

  await harness.request(admin, "POST", "/api/bunkers", { name: "Press" });

  const response = await harness.request(admin, "POST", "/api/bunkers/press/connections", {
    name: "CI",
    permissions: ["sign_event:1"],
  });

  assert.equal(response.status, 201);
  const body = await response.json();

  assert.equal(body.uri, minted.uri);
  assert.equal(body.connection.name, "CI");
  assert.equal(body.connection.connected, false);
  assert.deepEqual(body.connection.permissions, ["sign_event:1"]);
  assert.equal(calls[0].createdBy, admin.pubkey);

  // The claim token is in the URI and nowhere else, so listing can't leak it.
  const listed = await (await harness.request(admin, "GET", "/api/bunkers/press/connections")).text();
  assert.ok(!listed.includes("abcd"));
  assert.ok(!listed.includes("sealed"));
});

test("checks permission specs before troubling the gateway", async () => {
  let minted = false;
  const { harness, admin } = await setup(
    fakeBunkers({
      available: true,
      mintConnection: async () => {
        minted = true;
        return { signer_pubkey: "cc".repeat(32), uri: null };
      },
    }),
  );

  await harness.request(admin, "POST", "/api/bunkers", { name: "Press" });

  const response = await harness.request(admin, "POST", "/api/bunkers/press/connections", {
    name: "CI",
    permissions: ["sign_psbt"],
  });

  assert.equal(response.status, 400);
  assert.match((await response.json()).error, /Unknown permission/);
  assert.equal(minted, false);
});

test("edits and revokes a connection", async () => {
  const { harness, admin } = await setup();
  const signerPubkey = "dd".repeat(32);

  const created = await (await harness.request(admin, "POST", "/api/bunkers", { name: "Press" })).json();
  const bunker = await harness.db
    .selectFrom("bunkers")
    .select("id")
    .where("slug", "=", created.bunker.slug)
    .executeTakeFirstOrThrow();

  await harness.db
    .insertInto("bunker_connections")
    .values({
      bunker_id: bunker.id,
      name: "CI",
      signer_secret: "sealed",
      signer_pubkey: signerPubkey,
      client_pubkey: "ee".repeat(32),
      secret: null,
      relays: "[]",
      permissions: JSON.stringify(["sign_event:1"]),
      created_by: admin.pubkey,
      created_at: Math.floor(Date.now() / 1000),
      last_used_at: null,
      last_error: null,
    })
    .execute();

  const patched = await harness.request(admin, "PATCH", `/api/bunkers/press/connections/${signerPubkey}`, {
    name: "Deploy bot",
    permissions: ["sign_event:1", "sign_event:30023"],
  });

  const { connection } = await patched.json();
  assert.equal(connection.name, "Deploy bot");
  assert.deepEqual(connection.permissions, ["sign_event:1", "sign_event:30023"]);
  assert.equal(connection.connected, true);

  const bad = await harness.request(admin, "PATCH", `/api/bunkers/press/connections/${signerPubkey}`, {
    permissions: ["nip44_decrypt:4"],
  });
  assert.equal(bad.status, 400);

  assert.equal((await harness.request(admin, "DELETE", `/api/bunkers/press/connections/${signerPubkey}`)).status, 200);
  assert.deepEqual(await harness.db.selectFrom("bunker_connections").selectAll().execute(), []);
});

test("won't reach a connection through the wrong bunker", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/bunkers", { name: "Alpha" });
  await harness.request(admin, "POST", "/api/bunkers", { name: "Beta" });

  const alpha = await harness.db
    .selectFrom("bunkers")
    .select("id")
    .where("slug", "=", "alpha")
    .executeTakeFirstOrThrow();
  const signerPubkey = "dd".repeat(32);

  await harness.db
    .insertInto("bunker_connections")
    .values({
      bunker_id: alpha.id,
      name: "CI",
      signer_secret: "sealed",
      signer_pubkey: signerPubkey,
      client_pubkey: null,
      secret: null,
      relays: "[]",
      permissions: "[]",
      created_by: admin.pubkey,
      created_at: Math.floor(Date.now() / 1000),
      last_used_at: null,
      last_error: null,
    })
    .execute();

  const wrong = await harness.request(admin, "DELETE", `/api/bunkers/beta/connections/${signerPubkey}`);
  assert.equal(wrong.status, 404);
});

test("records who did what", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/bunkers", { name: "Press" });
  await harness.request(admin, "PATCH", "/api/bunkers/press", { name: "Newsroom" });
  await harness.request(admin, "DELETE", "/api/bunkers/press");

  const entries = await harness.db.selectFrom("audit_log").selectAll().orderBy("id").execute();

  assert.deepEqual(
    entries.map((entry) => entry.action),
    ["bunker.create", "bunker.update", "bunker.delete"],
  );
  assert.ok(entries.every((entry) => entry.actor_pubkey === admin.pubkey));
});
