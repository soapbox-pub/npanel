import assert from "node:assert/strict";
import { after, test } from "node:test";
import { nip19 } from "nostr-tools";
import { NAMED_SITE_KIND, ROOT_SITE_KIND } from "../nsite-address.ts";
import { type Harness, createHarness, testUser } from "../test/harness.ts";

const harnesses: Harness[] = [];

after(async () => {
  for (const harness of harnesses) await harness.cleanup();
});

async function setup() {
  const harness = await createHarness();
  harnesses.push(harness);

  const admin = testUser();
  const staff = testUser();
  const stranger = testUser();

  await harness.addUser(admin, "admin");
  await harness.addUser(staff, "staff");

  return { harness, admin, staff, stranger };
}

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const NPUB = nip19.npubEncode(PUBKEY);

test("rejects unauthenticated and unknown users", async () => {
  const { harness, stranger } = await setup();

  const anonymous = await harness.request(undefined, "GET", "/api/me");
  assert.equal(anonymous.status, 401);

  const unknown = await harness.request(stranger, "GET", "/api/me");
  assert.equal(unknown.status, 403);
  assert.match((await unknown.json()).error, /no access/);
});

test("identifies the signed-in user", async () => {
  const { harness, admin } = await setup();

  const response = await harness.request(admin, "GET", "/api/me");
  assert.equal(response.status, 200);

  const body = await response.json();
  assert.equal(body.pubkey, admin.pubkey);
  assert.equal(body.role, "admin");
  assert.equal(body.npub, nip19.npubEncode(admin.pubkey));
});

test("creates a host from any address format and stores one canonical form", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "Docs.Example.com",
    address: `https://${NPUB}.nsite.lol/some/page`,
  });

  assert.equal(created.status, 201);
  const { host } = await created.json();
  assert.equal(host.hostname, "docs.example.com");
  assert.equal(host.pubkey, PUBKEY);
  assert.equal(host.kind, ROOT_SITE_KIND);
  assert.equal(host.identifier, "");
  assert.equal(host.address, `${ROOT_SITE_KIND}:${PUBKEY}:`);
  assert.equal(host.enabled, true);
  assert.equal(host.active, null);

  const listed = await harness.request(admin, "GET", "/api/hosts");
  assert.equal((await listed.json()).hosts.length, 1);
});

test("rejects bad hostnames, bad addresses, and duplicates", async () => {
  const { harness, admin } = await setup();

  const badHost = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "not a hostname",
    address: NPUB,
  });
  assert.equal(badHost.status, 400);
  assert.match((await badHost.json()).error, /valid hostname/);

  const badAddress = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: "hello world",
  });
  assert.equal(badAddress.status, 400);

  const dashboard = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "panel.test",
    address: NPUB,
  });
  assert.equal(dashboard.status, 409);

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });
  const duplicate = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  assert.equal(duplicate.status, 409);
  assert.match((await duplicate.json()).error, /already configured/);
});

test("staff only see hosts they are granted", async () => {
  const { harness, admin, staff } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "admin-only.example.com",
    address: NPUB,
  });
  const adminHost = (await created.json()).host;

  const empty = await harness.request(staff, "GET", "/api/hosts");
  assert.deepEqual((await empty.json()).hosts, []);

  const forbidden = await harness.request(staff, "GET", `/api/hosts/${adminHost.id}`);
  assert.equal(forbidden.status, 403);

  const granted = await harness.request(admin, "PUT", `/api/hosts/${adminHost.id}/grants`, {
    pubkeys: [staff.pubkey],
  });
  assert.equal(granted.status, 200);

  const visible = await harness.request(staff, "GET", "/api/hosts");
  assert.equal((await visible.json()).hosts.length, 1);

  const allowed = await harness.request(staff, "GET", `/api/hosts/${adminHost.id}`);
  assert.equal(allowed.status, 200);
});

test("staff can manage the hosts they create", async () => {
  const { harness, staff } = await setup();

  const created = await harness.request(staff, "POST", "/api/hosts", {
    hostname: "team.example.com",
    address: NPUB,
  });
  assert.equal(created.status, 201);

  const mine = await harness.request(staff, "GET", "/api/hosts");
  assert.equal((await mine.json()).hosts.length, 1);
});

test("grants must name existing users", async () => {
  const { harness, admin, stranger } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;

  const response = await harness.request(admin, "PUT", `/api/hosts/${host.id}/grants`, {
    pubkeys: [stranger.pubkey],
  });
  assert.equal(response.status, 400);
  assert.match((await response.json()).error, /Not npanel users/);
});

test("retargets a host and clears its pin", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;

  const updated = await harness.request(admin, "PATCH", `/api/hosts/${host.id}`, {
    address: `${NAMED_SITE_KIND}:${PUBKEY}:docs`,
  });

  assert.equal(updated.status, 200);
  const patched = (await updated.json()).host;
  assert.equal(patched.kind, NAMED_SITE_KIND);
  assert.equal(patched.identifier, "docs");
  assert.equal(patched.pinned_event_id, null);
});

test("refuses to pin a version from another site", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;

  await harness.db
    .insertInto("site_events")
    .values({
      event_id: "a".repeat(64),
      pubkey: "b".repeat(64),
      kind: ROOT_SITE_KIND,
      identifier: "",
      created_at: 1,
      event: "{}",
      status: "ready",
      files_total: 0,
      files_ready: 0,
      error: null,
      fetched_at: 1,
      ready_at: 1,
    })
    .execute();

  const response = await harness.request(admin, "PATCH", `/api/hosts/${host.id}`, {
    pinned_event_id: "a".repeat(64),
  });

  assert.equal(response.status, 400);
  assert.match((await response.json()).error, /different site/);
});

test("deletes a host", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;

  const deleted = await harness.request(admin, "DELETE", `/api/hosts/${host.id}`);
  assert.equal(deleted.status, 200);

  const missing = await harness.request(admin, "GET", `/api/hosts/${host.id}`);
  assert.equal(missing.status, 404);
});

test("only admins manage users and settings", async () => {
  const { harness, staff } = await setup();

  assert.equal((await harness.request(staff, "GET", "/api/users")).status, 403);
  assert.equal((await harness.request(staff, "GET", "/api/settings")).status, 403);
  assert.equal((await harness.request(staff, "GET", "/api/audit-log")).status, 403);
});

test("adds, promotes, and removes users", async () => {
  const { harness, admin, stranger } = await setup();

  const created = await harness.request(admin, "POST", "/api/users", {
    pubkey: nip19.npubEncode(stranger.pubkey),
    role: "staff",
  });
  assert.equal(created.status, 201);
  assert.equal((await created.json()).user.pubkey, stranger.pubkey);

  const promoted = await harness.request(admin, "PATCH", `/api/users/${stranger.pubkey}`, {
    role: "admin",
  });
  assert.equal(promoted.status, 200);
  assert.equal((await promoted.json()).user.role, "admin");

  const removed = await harness.request(admin, "DELETE", `/api/users/${stranger.pubkey}`);
  assert.equal(removed.status, 200);

  const listed = await harness.request(admin, "GET", "/api/users");
  const pubkeys = (await listed.json()).users.map((user: { pubkey: string }) => user.pubkey);
  assert.ok(!pubkeys.includes(stranger.pubkey));
});

test("protects the last admin", async () => {
  const { harness, admin } = await setup();

  const demoted = await harness.request(admin, "PATCH", `/api/users/${admin.pubkey}`, { role: "staff" });
  assert.equal(demoted.status, 409);
  assert.match((await demoted.json()).error, /last admin/);

  const deleted = await harness.request(admin, "DELETE", `/api/users/${admin.pubkey}`);
  assert.equal(deleted.status, 409);
});

test("updates settings, validates them, and audits the change", async () => {
  const { harness, admin } = await setup();

  const rejected = await harness.request(admin, "PUT", "/api/settings", { relays: ["https://not-a-relay"] });
  assert.equal(rejected.status, 400);

  const unknown = await harness.request(admin, "PUT", "/api/settings", { nonsense: 1 });
  assert.equal(unknown.status, 400);
  assert.match((await unknown.json()).error, /Unknown settings/);

  const updated = await harness.request(admin, "PUT", "/api/settings", {
    relays: ["wss://relay.example.com"],
    resync_interval: 900,
  });
  assert.equal(updated.status, 200);

  const body = await updated.json();
  assert.deepEqual(body.settings.relays, ["wss://relay.example.com"]);
  assert.deepEqual(body.changed.sort(), ["relays", "resync_interval"]);

  const audit = await harness.request(admin, "GET", "/api/audit-log");
  const entries = (await audit.json()).entries;
  assert.equal(entries.length, 2);
  assert.ok(entries.every((entry: { action: string }) => entry.action === "settings.update"));
});

test("audits host changes", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;
  await harness.request(admin, "DELETE", `/api/hosts/${host.id}`);

  const audit = await harness.request(admin, "GET", "/api/audit-log");
  const actions = (await audit.json()).entries.map((entry: { action: string }) => entry.action);

  assert.deepEqual(actions, ["host.delete", "host.create"]);
});

test("tls-check answers for configured hosts only", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });

  const unknown = await harness.app.fetch(
    new Request("http://127.0.0.1/api/tls-check?domain=evil.example.com", {
      headers: { host: "panel.test" },
    }),
  );
  assert.equal(unknown.status, 404);

  const known = await harness.app.fetch(
    new Request("http://127.0.0.1/api/tls-check?domain=docs.example.com", {
      headers: { host: "panel.test" },
    }),
  );
  assert.equal(known.status, 200);

  const dashboard = await harness.app.fetch(
    new Request("http://127.0.0.1/api/tls-check?domain=panel.test", {
      headers: { host: "panel.test" },
    }),
  );
  assert.equal(dashboard.status, 200);
});

test("a token signed for one route does not work on another", async () => {
  const { harness, admin } = await setup();

  const response = await harness.app.fetch(
    new Request("http://127.0.0.1/api/users", {
      headers: { host: "panel.test", authorization: admin.auth("GET", "/api/me") },
    }),
  );

  assert.equal(response.status, 401);
});
