import assert from "node:assert/strict";
import { after, test } from "node:test";
import { nip19 } from "nostr-tools";
import { Logger } from "./log.ts";
import { DASHBOARD_HOST, type Harness, createHarness, testUser } from "./test/harness.ts";
import { type TlsCheckServer, startTlsCheckServer } from "./tls-check.ts";

const NPUB = nip19.npubEncode("3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d");

const harnesses: Harness[] = [];
const servers: TlsCheckServer[] = [];

after(async () => {
  for (const server of servers) await server.stop();
  for (const harness of harnesses) await harness.cleanup();
});

/**
 * The endpoint on a real socket, the way Caddy reaches it: its own listener,
 * with no hostname routing in front of it.
 */
async function setup() {
  const harness = await createHarness();
  harnesses.push(harness);

  const admin = testUser();
  await harness.addUser(admin, "admin");

  const server = startTlsCheckServer(
    { db: harness.db, settings: harness.settings, log: new Logger("error"), dashboardHost: DASHBOARD_HOST },
    // Port 0: the kernel picks a free one, so tests never collide.
    { port: 0, address: "127.0.0.1" },
  );
  servers.push(server);

  const base = `http://127.0.0.1:${server.port}`;

  return {
    harness,
    admin,
    base,
    /** What Caddy's `ask` sends: a plain GET carrying no hostname of interest. */
    ask: (domain: string) => fetch(`${base}/tls-check?domain=${domain}`),
    get: (path: string) => fetch(`${base}${path}`),
    addHost: (hostname: string) => harness.request(admin, "POST", "/api/hosts", { hostname, address: NPUB }),
  };
}

test("answers for configured hosts only", async () => {
  const { ask, addHost } = await setup();

  await addHost("docs.example.com");

  const approved = await ask("docs.example.com");
  assert.equal(approved.status, 200);
  assert.equal(await approved.text(), "ok");

  assert.equal((await ask("evil.example.com")).status, 404);

  // The dashboard is a hostname npanel serves too, and needs a certificate
  // before anyone can log in to add the first site.
  assert.equal((await ask(DASHBOARD_HOST)).status, 200);
});

test("refuses a disabled host", async () => {
  const { ask, addHost, harness, admin } = await setup();

  const created = await addHost("docs.example.com");
  const host = (await created.json()).host;
  await harness.request(admin, "PATCH", `/api/hosts/${host.id}`, { enabled: false });

  // A hostname npanel answers 404 for has no use for a certificate.
  assert.equal((await ask("docs.example.com")).status, 404);
});

test("answers for the www a root domain redirects", async () => {
  const { ask, addHost } = await setup();

  await addHost("example.com");

  // The redirect is an HTTPS response like any other: without a certificate of
  // its own, the browser never gets far enough to be told where to go.
  assert.equal((await ask("www.example.com")).status, 200);

  // Nothing answers for this one, so nothing should have a certificate for it.
  assert.equal((await ask("www.other.com")).status, 404);
});

test("refuses www when the root domain doesn't redirect it", async () => {
  const { ask, addHost, harness, admin } = await setup();

  const created = await addHost("example.com");
  const host = (await created.json()).host;
  await harness.request(admin, "PATCH", `/api/hosts/${host.id}`, { redirect_www: false });

  assert.equal((await ask("www.example.com")).status, 404);
});

test("refuses the www of a subdomain", async () => {
  const { ask, addHost } = await setup();

  await addHost("docs.example.com");

  assert.equal((await ask("www.docs.example.com")).status, 404);
});

test("matches the hostname case-insensitively", async () => {
  const { ask, addHost } = await setup();

  await addHost("docs.example.com");

  // SNI is not normalized on the way in, and the proxy passes on what it got.
  assert.equal((await ask("DOCS.EXAMPLE.COM")).status, 200);
});

test("treats a missing domain as unknown", async () => {
  const { get } = await setup();

  assert.equal((await get("/tls-check")).status, 404);
  assert.equal((await get("/tls-check?domain=")).status, 404);
});

test("serves nothing but the one path", async () => {
  const { get } = await setup();

  // The dashboard and the JSON API stay on the public listener. This port
  // exists to answer one question.
  assert.equal((await get("/")).status, 404);
  assert.equal((await get("/api/hosts")).status, 404);
  assert.equal((await get("/api/tls-check?domain=panel.test")).status, 404);
});

test("refuses methods Caddy never sends", async () => {
  const { base } = await setup();

  const response = await fetch(`${base}/tls-check?domain=${DASHBOARD_HOST}`, { method: "POST" });

  assert.equal(response.status, 405);
  assert.equal(response.headers.get("allow"), "GET, HEAD");
});

test("stops authorizing a host that keeps asking", async () => {
  const { ask, addHost, harness, admin } = await setup();

  await addHost("docs.example.com");
  await harness.request(admin, "PUT", "/api/settings", { tls_check_limit_per_host: 2 });

  assert.equal((await ask("docs.example.com")).status, 200);
  assert.equal((await ask("docs.example.com")).status, 200);

  const throttled = await ask("docs.example.com");
  assert.equal(throttled.status, 429);
  assert.equal(throttled.headers.get("retry-after"), "60");

  // The dashboard is never throttled: an exhausted budget must not be able to
  // lock admins out of npanel itself.
  assert.equal((await ask(DASHBOARD_HOST)).status, 200);
});

test("refusals do not spend the budget", async () => {
  const { ask, addHost, harness, admin } = await setup();

  await addHost("docs.example.com");
  await harness.request(admin, "PUT", "/api/settings", { tls_check_limit_total: 2 });

  // Anyone can make the proxy ask about any name, so if refusals counted, a
  // stranger could starve certificates for the hosts that are configured.
  for (let i = 0; i < 20; i++) {
    assert.equal((await ask(`spray-${i}.example.com`)).status, 404);
  }

  assert.equal((await ask("docs.example.com")).status, 200);
});

test("caps authorizations across all hosts", async () => {
  const { ask, addHost, harness, admin } = await setup();

  await addHost("one.example.com");
  await addHost("two.example.com");
  await harness.request(admin, "PUT", "/api/settings", { tls_check_limit_total: 2 });

  assert.equal((await ask("one.example.com")).status, 200);
  assert.equal((await ask("two.example.com")).status, 200);
  assert.equal((await ask("one.example.com")).status, 429);
});

test("a throttled host does not drain the budget the others share", async () => {
  const { ask, addHost, harness, admin } = await setup();

  await addHost("broken.example.com");
  await addHost("fine.example.com");
  await harness.request(admin, "PUT", "/api/settings", {
    tls_check_limit_per_host: 1,
    tls_check_limit_total: 3,
  });

  assert.equal((await ask("broken.example.com")).status, 200);

  // One host stuck in a retry loop would otherwise spend the shared budget on
  // asks it is already being refused for.
  for (let i = 0; i < 10; i++) {
    assert.equal((await ask("broken.example.com")).status, 429);
  }

  assert.equal((await ask("fine.example.com")).status, 200);
});
