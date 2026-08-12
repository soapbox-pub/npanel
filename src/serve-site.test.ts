import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { after, test } from "node:test";
import { matchesEtag, parseRange } from "./serve-site.ts";
import { type Harness, createHarness } from "./test/harness.ts";

const harnesses: Harness[] = [];

after(async () => {
  for (const harness of harnesses) await harness.cleanup();
});

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

interface VersionOpts {
  eventId: string;
  createdAt?: number;
  status?: "ready" | "failed" | "fetching_blobs";
  files: Record<string, string>;
}

/** A harness with one host and, optionally, some deployed versions. */
async function setup(opts: { enabled?: boolean; versions?: VersionOpts[]; pin?: string } = {}) {
  const harness = await createHarness();
  harnesses.push(harness);

  const host = await harness.db
    .insertInto("hosts")
    .values({
      hostname: "docs.example.com",
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      pinned_event_id: opts.pin ?? null,
      enabled: opts.enabled === false ? 0 : 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .returning("id")
    .executeTakeFirstOrThrow();

  for (const version of opts.versions ?? []) {
    const entries = Object.entries(version.files);

    await harness.db
      .insertInto("site_events")
      .values({
        event_id: version.eventId,
        pubkey: PUBKEY,
        kind: 15128,
        identifier: "",
        created_at: version.createdAt ?? 1000,
        event: "{}",
        status: version.status ?? "ready",
        files_total: entries.length,
        files_ready: entries.length,
        error: null,
        fetched_at: 1,
        ready_at: 1,
      })
      .execute();

    for (const [path, content] of entries) {
      const hash = sha256(content);
      await harness.db.insertInto("site_files").values({ event_id: version.eventId, path, sha256: hash }).execute();
      await harness.blobs.put(hash, new TextEncoder().encode(content));
      await harness.db
        .insertInto("blobs")
        .values({ sha256: hash, size: content.length, seen_at: 1 })
        .onConflict((oc) => oc.column("sha256").doNothing())
        .execute();
    }
  }

  return { harness, hostId: host.id };
}

test("unknown hostnames are not served", async () => {
  const { harness } = await setup();

  const response = await harness.site("nope.example.com", "/");
  assert.equal(response.status, 404);
  assert.match(await response.text(), /not configured/);
});

test("a disabled host stops serving", async () => {
  const { harness } = await setup({
    enabled: false,
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "hi" } }],
  });

  assert.equal((await harness.site("docs.example.com", "/")).status, 404);
});

test("a host with nothing deployed yet says so", async () => {
  const { harness } = await setup();

  const response = await harness.site("docs.example.com", "/");
  assert.equal(response.status, 503);
  assert.equal(response.headers.get("retry-after"), "30");
});

test("a failed deploy does not become servable", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), status: "failed", files: { "/index.html": "broken" } }],
  });

  assert.equal((await harness.site("docs.example.com", "/")).status, 503);
});

test("serves index.html with caching headers", async () => {
  const body = "<h1>hello</h1>";
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": body } }],
  });

  const response = await harness.site("docs.example.com", "/");

  assert.equal(response.status, 200);
  assert.equal(await response.text(), body);
  assert.equal(response.headers.get("content-type"), "text/html; charset=utf-8");
  assert.equal(response.headers.get("etag"), `"${sha256(body)}"`);
  assert.equal(response.headers.get("content-length"), String(body.length));
  assert.equal(response.headers.get("cache-control"), "public, max-age=3600");
  assert.equal(response.headers.get("accept-ranges"), "bytes");
  assert.equal(response.headers.get("x-npanel-pubkey"), PUBKEY);
  assert.equal(response.headers.get("x-npanel-event"), "a".repeat(64));
});

test("serves nested paths and their index files", async () => {
  const { harness } = await setup({
    versions: [
      {
        eventId: "a".repeat(64),
        files: { "/index.html": "root", "/blog/index.html": "blog", "/app.js": "console.log(1)" },
      },
    ],
  });

  assert.equal(await (await harness.site("docs.example.com", "/blog")).text(), "blog");
  assert.equal(await (await harness.site("docs.example.com", "/blog/")).text(), "blog");

  const js = await harness.site("docs.example.com", "/app.js");
  assert.equal(js.headers.get("content-type"), "text/javascript; charset=utf-8");
});

test("honors If-None-Match", async () => {
  const body = "cached";
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": body } }],
  });

  const response = await harness.site("docs.example.com", "/", {
    headers: { "if-none-match": `"${sha256(body)}"` },
  });

  assert.equal(response.status, 304);
  assert.equal(await response.text(), "");
});

test("serves byte ranges", async () => {
  const body = "0123456789";
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/data.txt": body } }],
  });

  const partial = await harness.site("docs.example.com", "/data.txt", {
    headers: { range: "bytes=2-4" },
  });
  assert.equal(partial.status, 206);
  assert.equal(await partial.text(), "234");
  assert.equal(partial.headers.get("content-range"), "bytes 2-4/10");
  assert.equal(partial.headers.get("content-length"), "3");

  const unsatisfiable = await harness.site("docs.example.com", "/data.txt", {
    headers: { range: "bytes=50-60" },
  });
  assert.equal(unsatisfiable.status, 416);
  assert.equal(unsatisfiable.headers.get("content-range"), "bytes */10");
});

test("HEAD returns headers without a body", async () => {
  const body = "head me";
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": body } }],
  });

  const response = await harness.site("docs.example.com", "/", { method: "HEAD" });

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-length"), String(body.length));
  assert.equal(await response.text(), "");
});

test("falls back to the site's 404 page", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "root", "/404.html": "custom missing page" } }],
  });

  const response = await harness.site("docs.example.com", "/nope");
  assert.equal(response.status, 404);
  assert.equal(await response.text(), "custom missing page");
});

test("returns a plain 404 when the site has no 404 page", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "root" } }],
  });

  const response = await harness.site("docs.example.com", "/missing.html");
  assert.equal(response.status, 404);
  assert.match(await response.text(), /Not found/);
});

test("serves the newest ready version, or the pinned one", async () => {
  const versions: VersionOpts[] = [
    { eventId: "a".repeat(64), createdAt: 1000, files: { "/index.html": "v1" } },
    { eventId: "b".repeat(64), createdAt: 2000, files: { "/index.html": "v2" } },
  ];

  const newest = await setup({ versions });
  assert.equal(await (await newest.harness.site("docs.example.com", "/")).text(), "v2");

  const pinned = await setup({ versions, pin: "a".repeat(64) });
  assert.equal(await (await pinned.harness.site("docs.example.com", "/")).text(), "v1");
});

test("path traversal cannot escape the site", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "root" } }],
  });

  const response = await harness.site("docs.example.com", "/../../etc/passwd");
  // Normalizes to /etc/passwd, which the site does not contain.
  assert.equal(response.status, 404);
});

test("the dashboard host serves the SPA", async () => {
  const { harness } = await setup();

  const root = await harness.site("panel.test", "/");
  assert.equal(root.status, 200);
  assert.match(await root.text(), /npanel/);

  // Client-side routes fall back to index.html.
  const deep = await harness.site("panel.test", "/hosts/12");
  assert.equal(deep.status, 200);
  assert.match(await deep.text(), /npanel/);
});

test("X-Forwarded-Host decides which site is served", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "proxied" } }],
  });

  const response = await harness.app.fetch(
    new Request("http://127.0.0.1/", {
      headers: { host: "127.0.0.1:3000", "x-forwarded-host": "docs.example.com" },
    }),
  );

  assert.equal(await response.text(), "proxied");
});

test("parseRange handles the forms browsers send", () => {
  assert.deepEqual(parseRange("bytes=0-99", 1000), { start: 0, end: 99 });
  assert.deepEqual(parseRange("bytes=100-", 1000), { start: 100, end: 999 });
  assert.deepEqual(parseRange("bytes=-100", 1000), { start: 900, end: 999 });
  assert.deepEqual(parseRange("bytes=0-99999", 1000), { start: 0, end: 999 });
  assert.equal(parseRange(null, 1000), undefined);
  assert.equal(parseRange("bytes=0-99, 200-299", 1000), undefined, "multi-range is ignored");
  assert.equal(parseRange("bytes=1000-", 1000), "unsatisfiable");
  assert.equal(parseRange("bytes=500-400", 1000), "unsatisfiable");
});

test("matchesEtag compares weak and strong tags", () => {
  assert.equal(matchesEtag('"abc"', '"abc"'), true);
  assert.equal(matchesEtag('W/"abc"', '"abc"'), true);
  assert.equal(matchesEtag('"other", "abc"', '"abc"'), true);
  assert.equal(matchesEtag("*", '"abc"'), true);
  assert.equal(matchesEtag('"other"', '"abc"'), false);
  assert.equal(matchesEtag(null, '"abc"'), false);
});
