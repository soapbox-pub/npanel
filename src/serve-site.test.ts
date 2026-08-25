import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readdirSync } from "node:fs";
import { after, test } from "node:test";
import { brotliDecompressSync, gunzipSync } from "node:zlib";
import { requestScheme } from "./app.ts";
import { encodeNaddr } from "./nsite-address.ts";
import { matchesEtag, negotiateEncoding, parseRange } from "./serve-site.ts";
import { type Harness, createHarness } from "./test/harness.ts";

const harnesses: Harness[] = [];

after(async () => {
  for (const harness of harnesses) await harness.cleanup();
});

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";

/** How many descriptors this process holds. Linux only; the tests run there. */
function openDescriptors(): number {
  try {
    return readdirSync("/proc/self/fd").length;
  } catch {
    return 0;
  }
}

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
async function setup(opts: { enabled?: boolean; versions?: VersionOpts[]; pin?: string; spa?: boolean } = {}) {
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
      spa: opts.spa ? 1 : 0,
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

/** A harness whose one host is a root domain, which is what `www` needs. */
async function rootDomain(opts: { redirectWww?: boolean; enabled?: boolean } = {}) {
  const harness = await createHarness();
  harnesses.push(harness);

  await harness.db
    .insertInto("hosts")
    .values({
      hostname: "example.com",
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      pinned_event_id: null,
      enabled: opts.enabled === false ? 0 : 1,
      redirect_www: opts.redirectWww === false ? 0 : 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .execute();

  return harness;
}

test("www redirects to the root domain, path and query intact", async () => {
  const harness = await rootDomain();

  const response = await harness.site("www.example.com", "/docs/page?q=1");

  assert.equal(response.status, 301);
  // https without being told: npanel only ever sees the proxy's plain HTTP.
  assert.equal(response.headers.get("location"), "https://example.com/docs/page?q=1");
  // Long enough to be free, short enough that turning the redirect off works.
  assert.equal(response.headers.get("cache-control"), "public, max-age=3600");
});

test("www redirects to the scheme the proxy reports", async () => {
  const harness = await rootDomain();

  const response = await harness.site("www.example.com", "/", {
    headers: { "x-forwarded-proto": "http" },
  });

  assert.equal(response.headers.get("location"), "http://example.com/");
});

test("www is not served when the root domain doesn't redirect it", async () => {
  const harness = await rootDomain({ redirectWww: false });

  assert.equal((await harness.site("www.example.com", "/")).status, 404);
});

test("www is not served when the root domain is disabled", async () => {
  const harness = await rootDomain({ enabled: false });

  // A hostname npanel answers 404 for has nothing to redirect to.
  assert.equal((await harness.site("www.example.com", "/")).status, 404);
});

test("a www host of its own is served rather than redirected away", async () => {
  const harness = await rootDomain();

  await harness.db
    .insertInto("hosts")
    .values({
      hostname: "www.example.com",
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "different",
      pinned_event_id: null,
      enabled: 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .execute();

  // 503, not 301: the explicit mapping wins, and it has nothing deployed yet.
  assert.equal((await harness.site("www.example.com", "/")).status, 503);
});

test("www of a subdomain is nobody's to redirect", async () => {
  const { harness } = await setup();

  // `docs.example.com` is configured, but `www.docs.example.com` is a name in
  // a zone npanel knows nothing about.
  assert.equal((await harness.site("www.docs.example.com", "/")).status, 404);
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
  // A document's URL survives a redeploy, so it must revalidate every time.
  assert.equal(response.headers.get("cache-control"), "public, no-cache");
  assert.equal(response.headers.get("accept-ranges"), "bytes");
  assert.equal(response.headers.get("x-nsite-addr"), encodeNaddr({ pubkey: PUBKEY, kind: 15128, identifier: "" }));
});

test("assets may be cached, documents may not", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page", "/app.js": "code" } }],
  });

  const asset = await harness.site("docs.example.com", "/app.js");
  assert.match(asset.headers.get("cache-control") ?? "", /max-age=/);

  const document = await harness.site("docs.example.com", "/");
  assert.equal(document.headers.get("cache-control"), "public, no-cache");
});

test("only GET and HEAD reach a site", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page" } }],
  });

  const response = await harness.site("docs.example.com", "/", { method: "POST" });

  assert.equal(response.status, 405);
  assert.equal(response.headers.get("allow"), "GET, HEAD");
});

test("any origin may read a site", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page" } }],
  });

  const response = await harness.site("docs.example.com", "/", {
    headers: { origin: "https://reader.example" },
  });

  assert.equal(response.headers.get("access-control-allow-origin"), "*");
  // Without these a cross-origin fetch cannot revalidate or resume.
  const exposed = response.headers.get("access-control-expose-headers") ?? "";
  assert.match(exposed, /etag/);
  assert.match(exposed, /content-range/);
  assert.match(exposed, /x-nsite-addr/);
});

test("error and redirect responses carry CORS headers too", async () => {
  const { harness } = await setup();
  const root = await rootDomain();

  // A cross-origin caller has to be able to read the status it got.
  const missing = await harness.site("nope.example.com", "/");
  assert.equal(missing.headers.get("access-control-allow-origin"), "*");

  const undeployed = await harness.site("docs.example.com", "/");
  assert.equal(undeployed.status, 503);
  assert.equal(undeployed.headers.get("access-control-allow-origin"), "*");

  // CORS follows redirects, so the hop needs the header as much as the target.
  const redirect = await root.site("www.example.com", "/");
  assert.equal(redirect.status, 301);
  assert.equal(redirect.headers.get("access-control-allow-origin"), "*");
});

test("preflights are answered, plain OPTIONS is still a 405", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/data.txt": "0123456789" } }],
  });

  const flight = await harness.site("docs.example.com", "/data.txt", {
    method: "OPTIONS",
    headers: {
      origin: "https://reader.example",
      "access-control-request-method": "GET",
      "access-control-request-headers": "range",
    },
  });

  assert.equal(flight.status, 204);
  assert.equal(flight.headers.get("access-control-allow-origin"), "*");
  assert.equal(flight.headers.get("access-control-allow-methods"), "GET, HEAD, OPTIONS");
  assert.equal(flight.headers.get("access-control-allow-headers"), "range");

  const plain = await harness.site("docs.example.com", "/data.txt", { method: "OPTIONS" });
  assert.equal(plain.status, 405);
  assert.equal(plain.headers.get("allow"), "GET, HEAD");
});

test("a site nobody configured is still served under the policy", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page" } }],
  });

  const csp = (await harness.site("docs.example.com", "/")).headers.get("content-security-policy") ?? "";

  // Scripts from the site's own origin only: the directive the whole policy
  // exists for, and the one that can break a site that didn't expect it.
  assert.match(csp, /script-src 'self'(;|$)/);
  assert.match(csp, /default-src 'none'/);

  // A hostname npanel doesn't serve is still an origin a browser is on.
  const missing = await harness.site("nope.example.com", "/");
  assert.equal(missing.status, 404);
  assert.match(missing.headers.get("content-security-policy") ?? "", /default-src 'none'/);
});

test("a site can turn the policy off for itself alone", async () => {
  const { harness, hostId } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page" } }],
  });

  await harness.db
    .insertInto("hosts")
    .values({
      hostname: "other.example.com",
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      pinned_event_id: null,
      enabled: 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .execute();

  await harness.db.updateTable("hosts").set({ csp: 0 }).where("id", "=", hostId).execute();

  assert.equal((await harness.site("docs.example.com", "/")).headers.get("content-security-policy"), null);

  // The site next door keeps its own protection: there is no switch that
  // reaches both, which is the whole point of putting it on the host.
  const neighbour = await harness.site("other.example.com", "/");
  assert.match(neighbour.headers.get("content-security-policy") ?? "", /default-src 'none'/);
});

/** A site whose landing page declares a policy of its own. */
const DECLARED = "default-src 'self'; script-src 'self' 'unsafe-inline' https://cdn.example";

async function declaresPolicy() {
  return await setup({
    versions: [
      {
        eventId: "a".repeat(64),
        files: {
          "/index.html": `<!doctype html><meta http-equiv="Content-Security-Policy" content="${DECLARED}"><title>page</title>`,
          "/app.css": "body{color:red}",
        },
      },
    ],
  });
}

test("a site that declares its own policy is served under that one", async () => {
  const { harness } = await declaresPolicy();

  const page = await harness.site("docs.example.com", "/");
  assert.equal(page.headers.get("content-security-policy"), DECLARED);

  // The header covers the whole site, not just the document that declared it:
  // a policy that only reached index.html would leave every other response
  // under a different set of rules than the page that asked for them.
  const asset = await harness.site("docs.example.com", "/app.css");
  assert.equal(asset.status, 200);
  assert.equal(asset.headers.get("content-security-policy"), DECLARED);

  // Including the answers that never reach a file.
  const missing = await harness.site("docs.example.com", "/nope");
  assert.equal(missing.status, 404);
  assert.equal(missing.headers.get("content-security-policy"), DECLARED);
});

test("a site's own policy is what it says, loose directives and all", async () => {
  const { harness } = await declaresPolicy();

  const csp = (await harness.site("docs.example.com", "/")).headers.get("content-security-policy") ?? "";

  // The gateway's policy is gone rather than added to it. This is the whole
  // consequence of honouring a site's own: `script-src 'self'` no longer
  // applies, because the site said something else.
  assert.doesNotMatch(csp, /default-src 'none'/);
  assert.match(csp, /https:\/\/cdn\.example/);
});

test("turning the policy off beats a site declaring one", async () => {
  const { harness, hostId } = await declaresPolicy();

  await harness.db.updateTable("hosts").set({ csp: 0 }).where("id", "=", hostId).execute();

  assert.equal((await harness.site("docs.example.com", "/")).headers.get("content-security-policy"), null);
});

test("a site with no index.html of its own falls back to the gateway's policy", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/feed.xml": "<rss/>" } }],
  });

  const response = await harness.site("docs.example.com", "/feed.xml");
  assert.match(response.headers.get("content-security-policy") ?? "", /default-src 'none'/);
});

test("a new host is protected without being asked", async () => {
  const harness = await createHarness();
  harnesses.push(harness);

  const host = await harness.db
    .insertInto("hosts")
    .values({
      hostname: "fresh.example.com",
      pubkey: PUBKEY,
      kind: 15128,
      identifier: "",
      pinned_event_id: null,
      enabled: 1,
      created_by: PUBKEY,
      created_at: 1,
      updated_at: 1,
    })
    .returningAll()
    .executeTakeFirstOrThrow();

  // The column default, not something the API or the dashboard remembered to
  // send: a row inserted by hand is protected too.
  assert.equal(host.csp, 1);
});

test("last-modified never runs ahead of when the manifest was seen", async () => {
  const { harness } = await setup({
    versions: [
      // created_at far in the future: the author writes whatever they like.
      { eventId: "a".repeat(64), createdAt: 4_000_000_000, files: { "/index.html": "page" } },
    ],
  });

  const response = await harness.site("docs.example.com", "/");
  const modified = new Date(response.headers.get("last-modified") ?? "");

  assert.ok(!Number.isNaN(modified.getTime()), "a parseable date");
  assert.ok(modified.getTime() <= Date.now(), "not in the future");
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

/** A body with enough structure to compress well and clear the size floor. */
const COMPRESSIBLE = "<!doctype html><title>hi</title><p>compress me</p>".repeat(64);

/** A harness serving one compressible file that has its copies stored. */
async function setupCompressed(path = "/index.html", body = COMPRESSIBLE): Promise<Harness> {
  const { harness } = await setup({ versions: [{ eventId: "a".repeat(64), files: { [path]: body } }] });
  await harness.blobs.ensureVariants(sha256(body));
  return harness;
}

async function bytes(response: Response): Promise<Buffer> {
  return Buffer.from(await response.arrayBuffer());
}

test("serves brotli to a client that accepts it", async () => {
  const harness = await setupCompressed();

  const response = await harness.site("docs.example.com", "/", {
    headers: { "accept-encoding": "gzip, deflate, br, zstd" },
  });

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-encoding"), "br");
  assert.equal(response.headers.get("vary"), "accept-encoding");
  assert.equal(response.headers.get("etag"), `"${sha256(COMPRESSIBLE)}-br"`);

  const body = await bytes(response);
  assert.equal(brotliDecompressSync(body).toString(), COMPRESSIBLE);
  assert.equal(response.headers.get("content-length"), String(body.length));
  assert.ok(body.length < COMPRESSIBLE.length);
});

test("serves gzip to a client that cannot take brotli", async () => {
  // What Tor Browser sends reaching an onion: Firefox withholds `br` from
  // plain HTTP, so the visitors who gain most are the ones stuck on gzip.
  const harness = await setupCompressed();

  const response = await harness.site("docs.example.com", "/", {
    headers: { "accept-encoding": "gzip, deflate" },
  });

  assert.equal(response.headers.get("content-encoding"), "gzip");
  assert.equal(response.headers.get("etag"), `"${sha256(COMPRESSIBLE)}-gzip"`);

  const body = await bytes(response);
  assert.equal(gunzipSync(body).toString(), COMPRESSIBLE);
  assert.ok(body.length < COMPRESSIBLE.length);
});

test("a client that asks for nothing gets the blob itself", async () => {
  const harness = await setupCompressed();

  for (const headers of [undefined, { "accept-encoding": "identity" }, { "accept-encoding": "br;q=0, gzip;q=0" }]) {
    const response = await harness.site("docs.example.com", "/", { headers });

    assert.equal(response.headers.get("content-encoding"), null);
    assert.equal(response.headers.get("etag"), `"${sha256(COMPRESSIBLE)}"`);
    assert.equal(await response.text(), COMPRESSIBLE);
  }
});

test("vary is set even when there was nothing to negotiate", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/logo.png": "not really a png" } }],
  });

  const response = await harness.site("docs.example.com", "/logo.png", {
    headers: { "accept-encoding": "gzip, br" },
  });

  // Nothing was stored to negotiate with, but a cache still has to be told
  // that this URL is one where the request headers matter.
  assert.equal(response.headers.get("content-encoding"), null);
  assert.equal(response.headers.get("vary"), "accept-encoding");
});

test("revalidation is per encoding", async () => {
  const harness = await setupCompressed();
  const hash = sha256(COMPRESSIBLE);

  const matched = await harness.site("docs.example.com", "/", {
    headers: { "accept-encoding": "br", "if-none-match": `"${hash}-br"` },
  });
  assert.equal(matched.status, 304);
  assert.equal(matched.headers.get("content-encoding"), "br");

  // The tag of a copy the client is not being sent must not satisfy it, or a
  // cache holding brotli would answer a gzip-only client with a 304.
  const crossed = await harness.site("docs.example.com", "/", {
    headers: { "accept-encoding": "gzip", "if-none-match": `"${hash}-br"` },
  });
  assert.equal(crossed.status, 200);
  assert.equal(crossed.headers.get("content-encoding"), "gzip");
});

test("a range applies to the copy being served", async () => {
  const harness = await setupCompressed();

  const whole = await bytes(await harness.site("docs.example.com", "/", { headers: { "accept-encoding": "gzip" } }));

  const partial = await harness.site("docs.example.com", "/", {
    headers: { "accept-encoding": "gzip", range: "bytes=1-4" },
  });

  assert.equal(partial.status, 206);
  assert.equal(partial.headers.get("content-encoding"), "gzip");
  assert.equal(partial.headers.get("content-range"), `bytes 1-4/${whole.length}`);
  assert.equal(partial.headers.get("content-length"), "4");
  assert.deepEqual(await bytes(partial), whole.subarray(1, 5));
});

test("recorded sizes are served without asking the store for them", async () => {
  const harness = await setupCompressed();
  const hash = sha256(COMPRESSIBLE);
  const variants = await harness.blobs.variants(hash);
  assert.ok(variants?.br && variants.gzip);

  // What a deploy records. The harness's setup inserts rows with NULL sizes,
  // so every other test in this file exercises the fallback.
  await harness.db
    .updateTable("blobs")
    .set({ br_size: variants.br, gzip_size: variants.gzip })
    .where("sha256", "=", hash)
    .execute();

  harness.blobs.variants = async () => {
    throw new Error("variants() was called even though the row carries the sizes");
  };

  const response = await harness.site("docs.example.com", "/", {
    headers: { "accept-encoding": "br" },
  });

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-encoding"), "br");
  assert.equal(response.headers.get("content-length"), String(variants.br));
  assert.equal(brotliDecompressSync(await bytes(response)).toString(), COMPRESSIBLE);
});

test("a recorded absence is believed over the disk", async () => {
  // Sidecars exist on disk, but the row says none are kept: the row is the
  // deploy's settled answer, and negotiation goes by it alone.
  const harness = await setupCompressed();
  const hash = sha256(COMPRESSIBLE);

  await harness.db.updateTable("blobs").set({ br_size: -1, gzip_size: -1 }).where("sha256", "=", hash).execute();
  harness.blobs.variants = async () => {
    throw new Error("variants() was called even though the row carries the sizes");
  };

  const response = await harness.site("docs.example.com", "/", {
    headers: { "accept-encoding": "br, gzip" },
  });

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-encoding"), null);
  assert.equal(response.headers.get("etag"), `"${sha256(COMPRESSIBLE)}"`);
  assert.equal(await response.text(), COMPRESSIBLE);
});

test("HEAD reports the length of the copy it would have sent", async () => {
  const harness = await setupCompressed();

  const head = await harness.site("docs.example.com", "/", {
    method: "HEAD",
    headers: { "accept-encoding": "br" },
  });
  const get = await harness.site("docs.example.com", "/", { headers: { "accept-encoding": "br" } });

  assert.equal(head.headers.get("content-encoding"), "br");
  assert.equal(await head.text(), "");
  assert.equal(head.headers.get("content-length"), get.headers.get("content-length"));
});

test("negotiateEncoding prefers the smallest copy the client will take", async () => {
  const variants = { identity: 100, br: 60, gzip: 50 };

  // No stated preference: size decides, whichever way it falls.
  assert.equal(negotiateEncoding("gzip, br", variants), "gzip");
  assert.equal(negotiateEncoding("gzip, br", { identity: 100, br: 40, gzip: 50 }), "br");
  assert.equal(negotiateEncoding("*", variants), "gzip");

  // A stated preference outranks size, which is what `q` is for.
  assert.equal(negotiateEncoding("gzip;q=0.5, br", variants), "br");

  // Zero is a refusal, not a ranking.
  assert.equal(negotiateEncoding("gzip;q=0, br", variants), "br");
  assert.equal(negotiateEncoding("gzip;q=0, br;q=0", variants), undefined);

  assert.equal(negotiateEncoding(null, variants), undefined);
  assert.equal(negotiateEncoding("br", { identity: 100 }), undefined);
  assert.equal(negotiateEncoding("BR", variants), "br");
  assert.equal(negotiateEncoding("br;q=bogus", variants), "br");

  // Refusing identity with nothing else on offer still gets identity: a
  // static file server has no reason to answer 406.
  assert.equal(negotiateEncoding("identity;q=0", { identity: 100 }), undefined);
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

test("an SPA host serves its shell for a path the site has no file for", async () => {
  const { harness } = await setup({
    spa: true,
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "shell" } }],
  });

  for (const path of ["/about", "/users/alex", "/about.html"]) {
    const response = await harness.site("docs.example.com", path);
    assert.equal(response.status, 200, path);
    assert.equal(await response.text(), "shell");
  }
});

test("an SPA host prefers the shell the site published as 404.html", async () => {
  const { harness } = await setup({
    spa: true,
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "index", "/404.html": "shell" } }],
  });

  const deep = await harness.site("docs.example.com", "/about");
  assert.equal(deep.status, 200);
  assert.equal(await deep.text(), "shell");

  // The root is still a real file, not a fallback.
  const root = await harness.site("docs.example.com", "/");
  assert.equal(root.status, 200);
  assert.equal(await root.text(), "index");
});

test("an SPA host still 404s a missing asset", async () => {
  const { harness } = await setup({
    spa: true,
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "shell", "/404.html": "shell" } }],
  });

  // Answering these with the shell and a 200 would replace a missing file with
  // a syntax error in the browser.
  for (const path of ["/assets/app.js", "/assets/app.css", "/logo.png"]) {
    const response = await harness.site("docs.example.com", path);
    assert.equal(response.status, 404, path);
  }
});

test("a host without the SPA flag 404s a deep link", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "shell" } }],
  });

  const response = await harness.site("docs.example.com", "/about");
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

test("Host decides which site is served, and X-Forwarded-Host is ignored", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "served" } }],
  });

  // Anything that reaches this port can write that header, so believing it
  // would be handing out the dashboard hostname to whoever asked.
  const site = await harness.app.fetch(
    new Request("http://127.0.0.1/", {
      headers: { host: "docs.example.com", "x-forwarded-host": "panel.test" },
    }),
  );
  assert.equal(await site.text(), "served");

  // And a proxy that rewrites `Host` doesn't get to make up for it here: the
  // fix is to configure the proxy, not to believe a second header.
  const rewritten = await harness.app.fetch(
    new Request("http://127.0.0.1/", {
      headers: { host: "127.0.0.1:3000", "x-forwarded-host": "docs.example.com" },
    }),
  );
  assert.equal(rewritten.status, 404);
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

const ONION = `${"a".repeat(56)}.onion`;

/** A host that Tor has published, as the reconciler would leave it. */
async function withOnion(hostId: number, harness: Harness): Promise<void> {
  await harness.db
    .insertInto("onion_services")
    .values({ host_id: hostId, address: ONION, secret_key: "ED25519-V3:secret", created_at: 1 })
    .execute();
}

test("requestScheme takes the proxy's word, and assumes TLS without one", () => {
  const scheme = (proto?: string) =>
    requestScheme(new Request("http://127.0.0.1/", { headers: proto ? { "x-forwarded-proto": proto } : {} }));

  // Nothing said: a deployment behind TLS is the ordinary one, and guessing
  // wrong costs a redirect rather than a downgrade.
  assert.equal(scheme(), "https");

  // A proxy that did say is believed either way.
  assert.equal(scheme("https"), "https");
  assert.equal(scheme("http"), "http");

  // Caddy appends rather than replaces when a hop already set one.
  assert.equal(scheme("http, https"), "http");
});

test("a site is served at its onion address", async () => {
  const body = "<h1>hello</h1>";
  const { harness, hostId } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": body } }],
  });
  await withOnion(hostId, harness);

  const response = await harness.site(ONION, "/");

  assert.equal(response.status, 200);
  assert.equal(await response.text(), body);
});

test("a document over https advertises the site's onion address", async () => {
  const { harness, hostId } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/deep/page.html": "page", "/app.js": "code" } }],
  });
  await withOnion(hostId, harness);

  // The path and query are carried over, so the visitor lands where they were.
  const document = await harness.site("docs.example.com", "/deep/page?q=1", {
    headers: { "x-forwarded-proto": "https" },
  });
  assert.equal(document.headers.get("onion-location"), `http://${ONION}/deep/page?q=1`);

  // Not on an asset: the browser acts on this when it navigates.
  const asset = await harness.site("docs.example.com", "/app.js", { headers: { "x-forwarded-proto": "https" } });
  assert.equal(asset.headers.get("onion-location"), null);
});

test("the onion is advertised only where a browser will act on it", async () => {
  const { harness, hostId } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page" } }],
  });
  await withOnion(hostId, harness);

  // Over plain HTTP anything on the path could rewrite the header, so Tor
  // Browser ignores it and npanel does not pretend otherwise.
  const insecure = await harness.site("docs.example.com", "/", { headers: { "x-forwarded-proto": "http" } });
  assert.equal(insecure.headers.get("onion-location"), null);

  // And a visitor already on the onion is where the header would send them.
  const onion = await harness.site(ONION, "/");
  assert.equal(onion.headers.get("onion-location"), null);
});

test("a site with no onion advertises none", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page" } }],
  });

  const response = await harness.site("docs.example.com", "/", { headers: { "x-forwarded-proto": "https" } });
  assert.equal(response.headers.get("onion-location"), null);
});

/** Give a host some NIP-05 names, as the API would. */
async function withNames(
  hostId: number,
  harness: Harness,
  names: { name: string; pubkey: string; relays?: string[] }[],
): Promise<void> {
  for (const entry of names) {
    await harness.db
      .insertInto("nip05_names")
      .values({
        host_id: hostId,
        name: entry.name,
        pubkey: entry.pubkey,
        relays: JSON.stringify(entry.relays ?? []),
        created_at: 1,
        updated_at: 1,
      })
      .execute();
  }
}

test("a host with no names serves whatever the site published there", async () => {
  const { harness } = await setup({
    versions: [
      {
        eventId: "a".repeat(64),
        files: { "/index.html": "page", "/.well-known/nostr.json": '{"names":{"deployed":"ff"}}' },
      },
    ],
  });

  const response = await harness.site("docs.example.com", "/.well-known/nostr.json");

  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), { names: { deployed: "ff" } });
});

test("a host with no names and no file 404s, as any other path would", async () => {
  const { harness } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page" } }],
  });

  const response = await harness.site("docs.example.com", "/.well-known/nostr.json");

  assert.equal(response.status, 404);
});

test("stored names shadow the file the site deployed", async () => {
  const { harness, hostId } = await setup({
    versions: [
      {
        eventId: "a".repeat(64),
        files: { "/index.html": "page", "/.well-known/nostr.json": '{"names":{"deployed":"ff"}}' },
      },
    ],
  });
  await withNames(hostId, harness, [{ name: "alice", pubkey: PUBKEY, relays: ["wss://relay.example.com"] }]);

  const response = await harness.site("docs.example.com", "/.well-known/nostr.json");

  assert.equal(response.status, 200);
  assert.equal(response.headers.get("content-type"), "application/json; charset=utf-8");
  assert.deepEqual(await response.json(), {
    names: { alice: PUBKEY },
    relays: { [PUBKEY]: ["wss://relay.example.com"] },
  });
});

test("a name parameter narrows the answer to that one", async () => {
  const { harness, hostId } = await setup({
    versions: [{ eventId: "a".repeat(64), files: { "/index.html": "page" } }],
  });
  await withNames(hostId, harness, [
    { name: "alice", pubkey: PUBKEY },
    { name: "bob", pubkey: "b".repeat(64), relays: ["wss://relay.example.com"] },
  ]);

  const bob = await harness.site("docs.example.com", "/.well-known/nostr.json?name=BOB");
  assert.deepEqual(await bob.json(), {
    names: { bob: "b".repeat(64) },
    relays: { ["b".repeat(64)]: ["wss://relay.example.com"] },
  });

  // A name this domain doesn't know is an empty document, not a 404: the
  // question was answered.
  const stranger = await harness.site("docs.example.com", "/.well-known/nostr.json?name=carol");
  assert.equal(stranger.status, 200);
  assert.deepEqual(await stranger.json(), { names: {} });

  // And asking after nobody in particular lists everyone, which is what makes
  // a domain's names discoverable.
  const all = await harness.site("docs.example.com", "/.well-known/nostr.json");
  assert.deepEqual(Object.keys(((await all.json()) as { names: Record<string, string> }).names), ["alice", "bob"]);
});

test("names resolve before the site behind them has deployed", async () => {
  const { harness, hostId } = await setup();
  await withNames(hostId, harness, [{ name: "_", pubkey: PUBKEY }]);

  const names = await harness.site("docs.example.com", "/.well-known/nostr.json");
  assert.equal(names.status, 200);
  assert.deepEqual(await names.json(), { names: { _: PUBKEY } });

  // Everything else about the host is still waiting on that first deploy.
  const page = await harness.site("docs.example.com", "/");
  assert.equal(page.status, 503);
});

test("a disabled host answers for nobody", async () => {
  const { harness, hostId } = await setup({ enabled: false });
  await withNames(hostId, harness, [{ name: "alice", pubkey: PUBKEY }]);

  const response = await harness.site("docs.example.com", "/.well-known/nostr.json");

  assert.equal(response.status, 404);
});

test("names are readable by the JavaScript clients that look them up", async () => {
  const { harness, hostId } = await setup();
  await withNames(hostId, harness, [{ name: "alice", pubkey: PUBKEY }]);

  const response = await harness.site("docs.example.com", "/.well-known/nostr.json");
  assert.equal(response.headers.get("access-control-allow-origin"), "*");

  const head = await harness.site("docs.example.com", "/.well-known/nostr.json", { method: "HEAD" });
  assert.equal(head.status, 200);
  assert.equal(await head.text(), "");
  assert.equal(head.headers.get("content-length"), String(JSON.stringify({ names: { alice: PUBKEY } }).length));
});

test("a visitor who disconnects mid-response releases the descriptor", async () => {
  const { harness } = await setup();
  const bytes = new TextEncoder().encode("x".repeat(256 * 1024));
  const hash = sha256("x".repeat(256 * 1024));
  await harness.blobs.put(hash, bytes);

  const before = openDescriptors();

  // What a crawler does: take the first chunk, then go away.
  for (let i = 0; i < 200; i++) {
    const reader = harness.blobs.read(hash).getReader();
    await reader.read();
    await reader.cancel();
  }

  // Descriptors are closed on cancel, so this does not climb with the loop.
  assert.ok(
    openDescriptors() - before < 50,
    `descriptors grew by ${openDescriptors() - before} over 200 cancelled reads`,
  );
});

test("a blob read to the end is delivered whole", async () => {
  const { harness } = await setup();
  const content = "hello, world";
  const hash = sha256(content);
  await harness.blobs.put(hash, new TextEncoder().encode(content));

  const reader = harness.blobs.read(hash).getReader();
  const chunks: Uint8Array[] = [];
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
  }

  assert.equal(Buffer.concat(chunks).toString("utf8"), content);
});

test("a blob range is served inclusively, as HTTP counts it", async () => {
  const { harness } = await setup();
  const content = "0123456789";
  const hash = sha256(content);
  await harness.blobs.put(hash, new TextEncoder().encode(content));

  const reader = harness.blobs.read(hash, { range: { start: 2, end: 5 } }).getReader();
  const chunks: Uint8Array[] = [];
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
  }

  assert.equal(Buffer.concat(chunks).toString("utf8"), "2345");
});
