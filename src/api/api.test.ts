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

test("the seo flag is on unless turned off", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;
  assert.equal(host.seo, true, "generated sitemaps and canonical URLs cost a site nothing");

  const off = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, { seo: false });
  assert.equal((await off.json()).host.seo, false);

  const upfront = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "app.example.com",
    address: NPUB,
    seo: false,
  });
  assert.equal((await upfront.json()).host.seo, false);
});

test("analytics are off unless a host asks for them", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  assert.equal((await created.json()).host.analytics, false, "counting visitors is nobody's default");

  const on = await harness.request(admin, "PATCH", "/api/hosts/docs.example.com", { analytics: true });
  assert.equal((await on.json()).host.analytics, true);

  const audit = await harness.request(admin, "GET", "/api/audit-log");
  const entries = (await audit.json()).entries as { action: string; new_value: { analytics?: number } }[];
  assert.ok(entries.some((entry) => entry.action === "host.update" && entry.new_value.analytics === 1));
});

test("merging deployments is off unless a host asks for it", async () => {
  const { harness, admin } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;
  assert.equal(host.merge_deployments, false, "serving a file a deploy deleted is nobody's default");

  const on = await harness.request(admin, "PATCH", `/api/hosts/${host.hostname}`, { merge_deployments: true });
  assert.equal((await on.json()).host.merge_deployments, true);

  const upfront = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "app.example.com",
    address: NPUB,
    merge_deployments: true,
  });
  assert.equal((await upfront.json()).host.merge_deployments, true);
});

test("a merged host's file list is what it serves, not one manifest", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
    merge_deployments: true,
  });

  const deploys: [string, number, Record<string, string>][] = [
    ["a".repeat(64), 1000, { "/index.html": "1".repeat(64), "/old.js": "2".repeat(64) }],
    ["b".repeat(64), 2000, { "/index.html": "3".repeat(64) }],
  ];

  for (const [eventId, createdAt, paths] of deploys) {
    await harness.db
      .insertInto("site_events")
      .values({
        event_id: eventId,
        pubkey: PUBKEY,
        kind: ROOT_SITE_KIND,
        identifier: "",
        created_at: createdAt,
        event: "{}",
        status: "ready",
        files_total: Object.keys(paths).length,
        files_ready: Object.keys(paths).length,
        error: null,
        fetched_at: createdAt,
        ready_at: createdAt,
      })
      .execute();

    for (const [path, sha256] of Object.entries(paths)) {
      await harness.db.insertInto("site_files").values({ event_id: eventId, path, sha256 }).execute();
    }
  }
  await harness.builder.hostsChanged();

  const merged = await (await harness.request(admin, "GET", "/api/hosts/docs.example.com/files")).json();
  assert.equal(merged.event_id, "b".repeat(64));
  assert.deepEqual(
    merged.files.map((file: { path: string; sha256: string; inherited: boolean; source: string }) => [
      file.path,
      file.path.endsWith(".js") || file.path.endsWith(".html") ? file.sha256 : "",
      file.inherited,
      file.source,
    ]),
    [
      ["/index.html", "3".repeat(64), false, "site"],
      ["/old.js", "2".repeat(64), true, "site"],
      // What npanel made for it is part of what it serves, and says so.
      ["/robots.txt", "", false, "generated"],
      ["/sitemap.xml", "", false, "generated"],
    ],
  );

  // A version named outright is being inspected for itself.
  const one = await (
    await harness.request(admin, "GET", `/api/hosts/docs.example.com/files?event_id=${"b".repeat(64)}`)
  ).json();
  assert.deepEqual(
    one.files.map((file: { path: string }) => file.path),
    ["/index.html"],
  );
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
  await harness.builder.hostsChanged();

  const { sites } = await (await harness.request(undefined, "GET", "/api/sites")).json();

  // Both hosts point at the same site, so both get the same card — and the
  // relative image resolves to whichever hostname is serving it.
  assert.deepEqual(sites, [
    {
      hostname: "bare.example.com",
      title: "The Docs",
      description: "Everything about the thing.",
      image: "https://bare.example.com/cover.png",
      icon: null,
    },
    {
      hostname: "docs.example.com",
      title: "The Docs",
      description: "Everything about the thing.",
      image: "https://docs.example.com/cover.png",
      icon: null,
    },
  ]);
});

test("a site with nothing deployed is still listed", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "PUT", "/api/settings", { public_site_list: true });
  await harness.request(admin, "POST", "/api/hosts", { hostname: "soon.example.com", address: NPUB });

  const { sites } = await (await harness.request(undefined, "GET", "/api/sites")).json();

  assert.deepEqual(sites, [{ hostname: "soon.example.com", title: null, description: null, image: null, icon: null }]);
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

test("a host reports which policy it is actually served under", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });

  const detail = async () => (await (await harness.request(admin, "GET", "/api/hosts/docs.example.com")).json()).host;

  // Nothing deployed, so there is no site to have declared anything.
  const bare = await detail();
  assert.equal(bare.csp_source, "gateway");
  assert.match(bare.csp_policy, /default-src 'none'/);

  const index = `<!doctype html><meta http-equiv="Content-Security-Policy" content="default-src 'self'">`;
  const sha256 = createHash("sha256").update(index).digest("hex");
  const eventId = "a".repeat(64);

  await harness.db
    .insertInto("site_events")
    .values({
      event_id: eventId,
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
  await harness.db.insertInto("site_files").values({ event_id: eventId, path: "/index.html", sha256 }).execute();
  await harness.blobs.put(sha256, new TextEncoder().encode(index));
  await harness.builder.hostsChanged();

  const declared = await detail();
  assert.equal(declared.csp_source, "site");
  assert.equal(declared.csp_policy, "default-src 'self'");

  // The flag still decides whether anything is sent at all.
  await harness.request(admin, "PATCH", "/api/hosts/docs.example.com", { csp: false });

  const off = await detail();
  assert.equal(off.csp_source, "off");
  assert.equal(off.csp_policy, null);
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

test("manages a host's NIP-05 names", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });

  const empty = await harness.request(admin, "GET", "/api/hosts/docs.example.com/nip05");
  assert.equal(empty.status, 200);
  assert.deepEqual((await empty.json()).names, []);

  const created = await harness.request(admin, "PUT", "/api/hosts/docs.example.com/nip05/Bob", {
    pubkey: NPUB,
    relays: ["wss://relay.example.com/", "wss://relay.example.com"],
  });
  assert.equal(created.status, 201);

  const [name] = (await created.json()).names;
  assert.equal(name.name, "bob");
  // Stored as hex, which the NIP requires, and the relay written two ways is
  // one relay.
  assert.equal(name.pubkey, PUBKEY);
  assert.deepEqual(name.relays, ["wss://relay.example.com"]);

  // The same name again replaces it rather than adding a second row.
  const updated = await harness.request(admin, "PUT", "/api/hosts/docs.example.com/nip05/bob", {
    pubkey: NPUB,
    relays: [],
  });
  assert.equal(updated.status, 200);
  const names = (await updated.json()).names;
  assert.equal(names.length, 1);
  assert.deepEqual(names[0].relays, []);

  const removed = await harness.request(admin, "DELETE", "/api/hosts/docs.example.com/nip05/bob");
  assert.equal(removed.status, 200);
  assert.deepEqual((await removed.json()).names, []);

  const missing = await harness.request(admin, "DELETE", "/api/hosts/docs.example.com/nip05/bob");
  assert.equal(missing.status, 404);
});

test("a NIP-05 name has to be one a client could look up", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });

  const spaced = await harness.request(admin, "PUT", "/api/hosts/docs.example.com/nip05/bo%20b", {
    pubkey: NPUB,
  });
  assert.equal(spaced.status, 400);
  assert.match((await spaced.json()).error, /letters, digits/);

  const notAPubkey = await harness.request(admin, "PUT", "/api/hosts/docs.example.com/nip05/bob", {
    pubkey: "who?",
  });
  assert.equal(notAPubkey.status, 400);
  assert.match((await notAPubkey.json()).error, /valid pubkey/);

  const notARelay = await harness.request(admin, "PUT", "/api/hosts/docs.example.com/nip05/bob", {
    pubkey: NPUB,
    relays: ["https://relay.example.com"],
  });
  assert.equal(notARelay.status, 400);
  assert.match((await notARelay.json()).error, /wss:\/\//);

  const tooMany = await harness.request(admin, "PUT", "/api/hosts/docs.example.com/nip05/bob", {
    pubkey: NPUB,
    relays: Array.from({ length: 11 }, (_, index) => `wss://relay${index}.example.com`),
  });
  assert.equal(tooMany.status, 400);
  assert.match((await tooMany.json()).error, /at most 10 relays/);
});

test("NIP-05 names follow the site's own access, not a role", async () => {
  const { harness, admin, staff } = await setup();

  const created = await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: NPUB,
  });
  const host = (await created.json()).host;

  const forbidden = await harness.request(staff, "GET", `/api/hosts/${host.hostname}/nip05`);
  assert.equal(forbidden.status, 403);

  await harness.request(admin, "PUT", `/api/hosts/${host.hostname}/grants`, { pubkeys: [staff.pubkey] });

  // A grant is the whole permission: no admin has to be in the loop to name
  // somebody on a site staff already run.
  const allowed = await harness.request(staff, "PUT", `/api/hosts/${host.hostname}/nip05/bob`, {
    pubkey: NPUB,
  });
  assert.equal(allowed.status, 201);

  const missingHost = await harness.request(admin, "GET", "/api/hosts/nope.example.com/nip05");
  assert.equal(missingHost.status, 404);
});

test("deleting a host takes its names with it", async () => {
  const { harness, admin } = await setup();

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });
  await harness.request(admin, "PUT", "/api/hosts/docs.example.com/nip05/bob", { pubkey: NPUB });
  await harness.request(admin, "DELETE", "/api/hosts/docs.example.com");

  const rows = await harness.db.selectFrom("nip05_names").selectAll().execute();
  assert.deepEqual(rows, []);
});

test("status counts hosts, ready versions, and blob bytes", async () => {
  const { harness, admin } = await setup();

  // sum() over an empty table is NULL in SQL; the endpoint must say 0.
  const empty = await (await harness.request(admin, "GET", "/api/status")).json();
  assert.equal(empty.hosts, 0);
  assert.equal(empty.ready_versions, 0);
  assert.equal(empty.blobs, 0);
  assert.equal(empty.blob_bytes, 0);

  await harness.request(admin, "POST", "/api/hosts", { hostname: "docs.example.com", address: NPUB });

  await harness.db
    .insertInto("site_events")
    .values(
      (["ready", "failed"] as const).map((status, i) => ({
        event_id: String(i).repeat(64),
        pubkey: PUBKEY,
        kind: ROOT_SITE_KIND,
        identifier: "",
        created_at: 1,
        event: "{}",
        status,
        files_total: 0,
        files_ready: 0,
        error: null,
        fetched_at: 1,
        ready_at: 1,
      })),
    )
    .execute();

  await harness.db
    .insertInto("blobs")
    .values([
      { sha256: "a".repeat(64), size: 100, seen_at: 1, br_size: null, gzip_size: null },
      { sha256: "b".repeat(64), size: 250, seen_at: 1, br_size: null, gzip_size: null },
    ])
    .execute();

  const body = await (await harness.request(admin, "GET", "/api/status")).json();
  assert.equal(body.hosts, 1);
  assert.equal(body.ready_versions, 1);
  assert.equal(body.blobs, 2);
  assert.equal(body.blob_bytes, 350);
});
