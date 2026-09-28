import assert from "node:assert/strict";
import { after, test } from "node:test";
import { nip19 } from "nostr-tools";
import { type Harness, createHarness, testUser } from "../test/harness.ts";

const harnesses: Harness[] = [];

after(async () => {
  for (const harness of harnesses) await harness.cleanup();
});

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const FIREFOX = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

async function setup() {
  const harness = await createHarness();
  harnesses.push(harness);

  const admin = testUser();
  const staff = testUser();
  await harness.addUser(admin, "admin");
  await harness.addUser(staff, "staff");

  await harness.request(admin, "POST", "/api/hosts", {
    hostname: "docs.example.com",
    address: nip19.npubEncode(PUBKEY),
    analytics: true,
  });

  return { harness, admin, staff };
}

const QUERY = { date_range: "day", metrics: ["visitors", "pageviews"], dimensions: [] };

test("a site's analytics answer Plausible's query endpoint", async () => {
  const { harness, admin } = await setup();
  await harness.analytics.start();

  const event = await fetch(`http://127.0.0.1:${harness.analytics.port}/api/event`, {
    method: "POST",
    headers: { "content-type": "text/plain", "user-agent": FIREFOX },
    body: JSON.stringify({ n: "pageview", u: "https://docs.example.com/", d: "docs.example.com" }),
  });
  assert.equal(event.status, 202);
  assert.equal(event.headers.get("x-plausible-dropped"), null);

  const response = await harness.request(admin, "POST", "/api/hosts/docs.example.com/stats/query", QUERY);
  assert.equal(response.status, 200);

  const body = await response.json();
  assert.deepEqual(body.results, [{ dimensions: [], metrics: [1, 1] }]);
  assert.equal(body.query.site_id, "docs.example.com");

  const live = await harness.request(admin, "GET", "/api/hosts/docs.example.com/stats/current-visitors");
  assert.equal(await live.json(), 1);

  const suggestions = await harness.request(
    admin,
    "GET",
    "/api/hosts/docs.example.com/stats/suggestions/page?period=day&q=",
  );
  assert.deepEqual(await suggestions.json(), [{ value: "/", label: "/" }]);
});

test("a bad query is a 400 that says what was wrong", async () => {
  const { harness, admin } = await setup();
  await harness.analytics.start();

  const response = await harness.request(admin, "POST", "/api/hosts/docs.example.com/stats/query", {
    ...QUERY,
    metrics: ["conversion_rate"],
  });

  assert.equal(response.status, 400);
  assert.deepEqual(await response.json(), { error: "Unknown metric 'conversion_rate'." });
});

test("analytics are for whoever may manage the site", async () => {
  const { harness, staff } = await setup();

  const denied = await harness.request(staff, "POST", "/api/hosts/docs.example.com/stats/query", QUERY);
  assert.equal(denied.status, 403);

  const missing = await harness.request(staff, "POST", "/api/hosts/nope.example.com/stats/query", QUERY);
  assert.equal(missing.status, 404);
});

test("analytics that aren't running say so", async () => {
  const { harness, admin } = await setup();

  const response = await harness.request(admin, "POST", "/api/hosts/docs.example.com/stats/query", QUERY);
  assert.equal(response.status, 503);
  assert.match((await response.json()).error, /starting/);
});
