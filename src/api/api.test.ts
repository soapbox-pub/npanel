import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { after, test } from "node:test";
import { nip19 } from "nostr-tools";
import { NAMED_SITE_KIND, ROOT_SITE_KIND } from "../nsite-address.ts";
import { type Harness, createHarness, fakeResolver, testUser } from "../test/harness.ts";

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

function hostnames(sites: { hostname: string }[]): string[] {
  return sites.map((site) => site.hostname);
}
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
  assert.match((await badHost.json()).error, /valid domain/);

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

  const forbidden = await harness.request(staff, "GET", `/api/hosts/${adminHost.hostname}`);
  assert.equal(forbidden.status, 403);

  const granted = await harness.request(admin, "PUT", `/api/hosts/${adminHost.hostname}/grants`, {
    pubkeys: [staff.pubkey],
  });
  assert.equal(granted.status, 200);

  const visible = await harness.request(staff, "GET", "/api/hosts");
  assert.equal((await visible.json()).hosts.length, 1);

  const allowed = await harness.request(staff, "GET", `/api/hosts/${adminHost.hostname}`);
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

  const response = await harness.request(admin, "PUT", `/api/hosts/${host.hostname}/grants`, {
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

  const updated = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, {
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

  const response = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, {
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

  const deleted = await harness.request(admin, "DELETE", `/api/hosts/${host.hostname}`);
  assert.equal(deleted.status, 200);

  const missing = await harness.request(admin, "GET", `/api/hosts/${host.hostname}`);
  assert.equal(missing.status, 404);
});

test("hosts are addressed by hostname, not by a row id", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;

  // The primary key stays in the database, so nothing outside can count rows
  // or guess at a host it was never told about.
  assert.ok(!("id" in host));
  assert.equal((await harness.request(admin, "GET", "/api/hosts/1")).status, 404);

  // Hostnames are stored lowercased, so a URL must match on the same terms.
  assert.equal((await harness.request(admin, "GET", "/api/hosts/DOCS.example.com")).status, 200);

  const renamed = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, {
    hostname: "blog.example.com",
  });
  assert.equal(renamed.status, 200);

  assert.equal((await harness.request(admin, "GET", "/api/hosts/docs.example.com")).status, 404);
  assert.equal((await harness.request(admin, "GET", "/api/hosts/blog.example.com")).status, 200);
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
  await harness.request(admin, "DELETE", `/api/hosts/${host.hostname}`);

  const audit = await harness.request(admin, "GET", "/api/audit-log");
  const actions = (await audit.json()).entries.map((entry: { action: string }) => entry.action);

  assert.deepEqual(actions, ["host.delete", "host.create"]);
});

test("instance is public, and names whoever runs it", async () => {
  const { harness, admin } = await setup();

  const before = await harness.request(undefined, "GET", "/api/instance");
  assert.equal(before.status, 200);
  assert.deepEqual(await before.json(), { contact_npub: null, dashboard_host: "panel.test" });

  await harness.request(admin, "PUT", "/api/settings", { contact_npub: PUBKEY });

  const after = await harness.request(undefined, "GET", "/api/instance");
  assert.equal(after.status, 200);
  assert.equal((await after.json()).contact_npub, NPUB);
});

test("lists no sites publicly until an admin says so", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });

  const closed = await harness.request(undefined, "GET", "/api/sites");
  assert.equal(closed.status, 200);
  assert.deepEqual(await closed.json(), { sites: [] });

  await harness.request(admin, "PUT", "/api/settings", { public_site_list: true });

  const open = await harness.request(undefined, "GET", "/api/sites");
  assert.deepEqual(hostnames((await open.json()).sites), ["docs.example.com"]);
});

test("the public list leaves out hidden and disabled sites", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "PUT", "/api/settings", { public_site_list: true });

  const secret = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "secret.example.com",
    address: NPUB,
    hidden: true,
  });
  assert.equal((await secret.json()).host.hidden, true);

  const off = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "off.example.com",
    address: NPUB,
  });
  await harness.request(admin, "PATCH", `/api/hosts/${(await off.json()).host.hostname}`, { enabled: false });

  const shown = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await shown.json()).host;
  assert.equal(host.hidden, false);

  const sites = await harness.request(undefined, "GET", "/api/sites");
  assert.deepEqual(hostnames((await sites.json()).sites), ["docs.example.com"]);

  // Hiding a site takes it off the list without taking it offline.
  await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, { hidden: true });
  assert.deepEqual(hostnames((await (await harness.request(undefined, "GET", "/api/sites")).json()).sites), []);
});

test("www redirects unless the request says otherwise", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;
  assert.equal(host.redirect_www, true, "on by default, which is what people expect of www");

  const off = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, { redirect_www: false });
  assert.equal((await off.json()).host.redirect_www, false);

  // The flag is kept on a host it can't act on, so renaming a subdomain to a
  // root domain later doesn't quietly leave www unserved.
  const sub = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  assert.equal((await sub.json()).host.redirect_www, true);
});

test("the SPA flag is off unless asked for", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;
  assert.equal(host.spa, false, "turning a 404 into a 200 is nobody's default");

  const on = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, { spa: true });
  assert.equal((await on.json()).host.spa, true);

  const off = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, { spa: false });
  assert.equal((await off.json()).host.spa, false);

  const upfront = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "app.example.com",
    address: NPUB,
    spa: true,
  });
  assert.equal((await upfront.json()).host.spa, true);
});

test("a listed site describes itself with its own page metadata", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "PUT", "/api/settings", { public_site_list: true });
  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });
  await harness.request(admin, "POST", "/api/hosts", { hostname: "bare.example.com", address: NPUB });

  const index = `<html><head>
    <title>Ignored when Open Graph says otherwise</title>
    <meta property="og:title" content="The Docs">
    <meta property="og:description" content="Everything about the thing.">
    <meta property="og:image" content="/cover.png">
  </head></html>`;
  const sha256 = createHash("sha256").update(index).digest("hex");

  await harness.db
    .insertInto("site_events")
    .values({
      event_id: "c".repeat(64),
      pubkey: PUBKEY,
      kind: ROOT_SITE_KIND,
      identifier: "",
      created_at: 1000,
      event: "{}",
      status: "ready",
      files_total: 1,
      files_ready: 1,
      error: null,
      fetched_at: 1,
      ready_at: 1,
    })
    .execute();
  await harness.db
    .insertInto("site_files")
    .values({ event_id: "c".repeat(64), path: "/index.html", sha256 })
    .execute();
  await harness.blobs.put(sha256, new TextEncoder().encode(index));

  const { sites } = await (await harness.request(undefined, "GET", "/api/sites")).json();

  // Both hosts point at the same site, so both get the same card — and the
  // relative image resolves to whichever hostname is serving it.
  assert.deepEqual(sites, [
    {
      hostname: "bare.example.com",
      title: "The Docs",
      description: "Everything about the thing.",
      image: "https://bare.example.com/cover.png",
    },
    {
      hostname: "docs.example.com",
      title: "The Docs",
      description: "Everything about the thing.",
      image: "https://docs.example.com/cover.png",
    },
  ]);
});

test("a site with nothing deployed is still listed", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "PUT", "/api/settings", { public_site_list: true });
  await harness.request(admin, "POST", "/api/hosts", { hostname: "soon.example.com", address: NPUB });

  const { sites } = await (await harness.request(undefined, "GET", "/api/sites")).json();

  assert.deepEqual(sites, [{ hostname: "soon.example.com", title: null, description: null, image: null }]);
});

test("staff are told what else the gateway serves, once the list is public", async () => {
  const { harness, admin, staff } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "elsewhere.example.com",
    address: NPUB,
  });
  const other = (await created.json()).host;

  const mine = await harness.request(staff, "POST", "/api/hosts", {
    hostname: "mine.example.com",
    address: NPUB,
  });
  assert.equal(mine.status, 201);

  const closed = await harness.request(staff, "GET", "/api/hosts");
  const before = await closed.json();
  assert.deepEqual(
    before.hosts.map((host: { hostname: string }) => host.hostname),
    ["mine.example.com"],
  );
  assert.deepEqual(before.other_sites, []);

  await harness.request(admin, "PUT", "/api/settings", { public_site_list: true });

  const open = await (await harness.request(staff, "GET", "/api/hosts")).json();
  // Only the site they can't manage: their own is already in `hosts`.
  assert.deepEqual(open.other_sites, [{ hostname: "elsewhere.example.com" }]);
  assert.equal(open.hosts.length, 1);

  // Naming a site is not access to it.
  assert.equal((await harness.request(staff, "GET", `/api/hosts/${other.hostname}`)).status, 403);

  await harness.request(admin, "PATCH", `/api/hosts/${other.hostname}`, { hidden: true });

  const hidden = await (await harness.request(staff, "GET", "/api/hosts")).json();
  assert.deepEqual(hidden.other_sites, []);
});

test("certificate authorization is not on the public API", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });

  // It answers on a listener of its own (src/tls-check.test.ts). Were it here
  // it would have to answer on every nsite hostname too, because Caddy asks
  // with no hostname to route on — which would let a stranger spend the
  // certificate budget of a host that is configured.
  const onDashboard = await harness.request(admin, "GET", "/api/tls-check?domain=docs.example.com");
  assert.equal(onDashboard.status, 404);
  assert.equal((await onDashboard.json()).error, "No such API route.");

  // On a site's own hostname the path is just a path: this is the site server
  // answering for a host with nothing deployed yet, not an authorization.
  const onSite = await harness.site("docs.example.com", "/api/tls-check?domain=docs.example.com");
  assert.equal(onSite.status, 503);
});

test("renaming a host cannot take over the dashboard hostname", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;

  const renamed = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, { hostname: "panel.test" });

  assert.equal(renamed.status, 409);
  assert.match((await renamed.json()).error, /dashboard itself/);
});

test("hostnames must sit under an allowed domain when one is set", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;

  await harness.request(admin, "PUT", "/api/settings", { allowed_host_suffixes: ["example.com"] });

  const outside = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "elsewhere.example.org",
    address: NPUB,
  });
  assert.equal(outside.status, 400);
  assert.match((await outside.json()).error, /outside the domains/);

  // A near-miss must not slip through on a bare string suffix match.
  const lookalike = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "notexample.com",
    address: NPUB,
  });
  assert.equal(lookalike.status, 400);

  const inside = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "blog.example.com",
    address: NPUB,
  });
  assert.equal(inside.status, 201);

  // The apex itself counts as being under its own suffix.
  const apex = await harness.request(admin, "POST", "/api/hosts", { hostname: "example.com", address: NPUB });
  assert.equal(apex.status, 201);

  const renamed = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, {
    hostname: "sneaky.example.org",
  });
  assert.equal(renamed.status, 400);
});

test("rejects domains that are not hostnames", async () => {
  const { harness, admin } = await setup();

  const response = await harness.request(admin, "PUT", "/api/settings", {
    allowed_host_suffixes: ["https://example.com/"],
  });

  assert.equal(response.status, 400);
  assert.match((await response.json()).error, /Not a valid domain/);
});

test("tells staff which DNS records point a hostname here", async () => {
  const harness = await createHarness({
    resolver: fakeResolver({ resolve4: async () => ["203.0.113.10"] }),
  });
  harnesses.push(harness);

  const staff = testUser();
  const admin = testUser();
  await harness.addUser(staff, "staff");
  await harness.addUser(admin, "admin");

  const detected = await harness.request(staff, "GET", "/api/dns");
  assert.equal(detected.status, 200);
  assert.deepEqual(await detected.json(), {
    records: [{ type: "A", value: "203.0.113.10" }],
    source: "dashboard_host",
    dashboard_host: "panel.test",
  });

  await harness.request(admin, "PUT", "/api/settings", { dns_targets: ["gateway.example.com"] });

  const configured = await (await harness.request(staff, "GET", "/api/dns")).json();
  assert.equal(configured.source, "settings");
  assert.deepEqual(configured.records, [{ type: "CNAME", value: "gateway.example.com" }]);

  assert.equal((await harness.request(undefined, "GET", "/api/dns")).status, 401);
});

test("unknown API routes answer with JSON, not the dashboard SPA", async () => {
  const { harness, admin } = await setup();

  const response = await harness.request(admin, "GET", "/api/nonexistent");

  assert.equal(response.status, 404);
  assert.match(response.headers.get("content-type") ?? "", /application\/json/);
  assert.match((await response.json()).error, /No such API route/);
});

test("refuses an oversized body before verifying anything", async () => {
  const { harness, stranger } = await setup();

  const body = JSON.stringify({ hostname: "x".repeat(2 * 1024 * 1024) });
  const response = await harness.app.fetch(
    new Request("http://127.0.0.1/api/hosts", {
      method: "POST",
      headers: {
        host: "panel.test",
        "content-type": "application/json",
        authorization: stranger.auth("POST", "/api/hosts", body),
      },
      body,
    }),
  );

  assert.equal(response.status, 413);
  assert.match((await response.json()).error, /too large/);
});

test("dashboard responses refuse to be framed", async () => {
  const { harness, admin } = await setup();

  const response = await harness.request(admin, "GET", "/api/me");

  assert.equal(response.headers.get("x-frame-options"), "DENY");
  assert.match(response.headers.get("content-security-policy") ?? "", /frame-ancestors 'none'/);
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
