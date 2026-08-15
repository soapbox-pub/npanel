import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { text } from "node:stream/consumers";
import { after, test } from "node:test";
import { type BlobDownloadError, BlobStore } from "./blobs.ts";
import { isPublicUrl } from "./net.ts";
import { serve } from "./test/serve.ts";

const dirs: string[] = [];

/**
 * A store in a fresh temp directory.
 *
 * Test Blossom servers listen on loopback, so by default downloads skip the
 * public-address check. Pass `isPublicUrl` to exercise the real one.
 */
async function store(isPublic: (url: URL) => Promise<boolean> = async () => true): Promise<BlobStore> {
  const dir = await mkdtemp(join(tmpdir(), "npanel-blobs-"));
  dirs.push(dir);
  return new BlobStore(dir, { isPublic });
}

after(async () => {
  for (const dir of dirs) await rm(dir, { recursive: true, force: true });
});

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

test("puts, finds, and reads back a blob", async () => {
  const blobs = await store();
  const hash = sha256("hello");

  assert.equal(await blobs.has(hash), false);
  assert.equal(await blobs.put(hash, new TextEncoder().encode("hello")), 5);
  assert.equal(await blobs.has(hash), true);
  assert.equal(await blobs.size(hash), 5);
  assert.equal(await text(blobs.read(hash)), "hello");
});

test("stores blobs in a two-level fanout", async () => {
  const blobs = await store();
  const hash = sha256("fanout");
  const path = blobs.path(hash);
  assert.ok(path.endsWith(join(hash.slice(0, 2), hash.slice(2, 4), hash)));
});

test("rejects bytes that do not match the hash", async () => {
  const blobs = await store();
  const hash = sha256("hello");

  await assert.rejects(() => blobs.put(hash, new TextEncoder().encode("goodbye")), /hash|Expected/);
  assert.equal(await blobs.has(hash), false);
});

test("reads byte ranges", async () => {
  const blobs = await store();
  const hash = sha256("0123456789");
  await blobs.put(hash, new TextEncoder().encode("0123456789"));

  assert.equal(await text(blobs.read(hash, { start: 2, end: 4 })), "234");
});

test("downloads and verifies a blob", async () => {
  const blobs = await store();
  const hash = sha256("downloaded");
  const server = serve(() => new Response("downloaded"));

  try {
    const size = await blobs.download(`${server.url}/${hash}`, hash, { maxSize: 1024 });
    assert.equal(size, 10);
    assert.equal(await text(blobs.read(hash)), "downloaded");
  } finally {
    await server.close();
  }
});

test("a lying server leaves nothing behind", async () => {
  const blobs = await store();
  const hash = sha256("expected");
  const server = serve(() => new Response("something else"));

  try {
    await assert.rejects(
      () => blobs.download(`${server.url}/${hash}`, hash, { maxSize: 1024 }),
      (error: BlobDownloadError) => {
        assert.equal(error.reason, "hash_mismatch");
        return true;
      },
    );
    assert.equal(await blobs.has(hash), false);
  } finally {
    await server.close();
  }
});

test("surfaces HTTP failures with their status", async () => {
  const blobs = await store();
  const hash = sha256("missing");
  const server = serve(() => new Response("nope", { status: 404 }));

  try {
    await assert.rejects(
      () => blobs.download(`${server.url}/${hash}`, hash, { maxSize: 1024 }),
      (error: BlobDownloadError) => {
        assert.equal(error.reason, "http");
        assert.equal(error.status, 404);
        return true;
      },
    );
  } finally {
    await server.close();
  }
});

test("refuses blobs larger than the limit, declared or not", async () => {
  const blobs = await store();
  const body = "x".repeat(4096);
  const hash = sha256(body);

  const declared = serve(() => new Response(body, { headers: { "content-length": String(body.length) } }));
  try {
    await assert.rejects(
      () => blobs.download(`${declared.url}/${hash}`, hash, { maxSize: 100 }),
      (error: BlobDownloadError) => {
        assert.equal(error.reason, "too_large");
        return true;
      },
    );
  } finally {
    await declared.close();
  }

  // Chunked response: no content-length, so the cap has to bite mid-stream.
  // A streamed body declares no length, so only the running total can catch it.
  const chunked = serve(
    () =>
      new Response(
        new ReadableStream({
          start(controller) {
            const encoder = new TextEncoder();
            controller.enqueue(encoder.encode(body.slice(0, 2048)));
            controller.enqueue(encoder.encode(body.slice(2048)));
            controller.close();
          },
        }),
      ),
  );
  try {
    await assert.rejects(
      () => blobs.download(`${chunked.url}/${hash}`, hash, { maxSize: 100 }),
      (error: BlobDownloadError) => {
        assert.equal(error.reason, "too_large");
        return true;
      },
    );
    assert.equal(await blobs.has(hash), false);
  } finally {
    await chunked.close();
  }
});

test("reports network failures instead of throwing raw fetch errors", async () => {
  const blobs = await store();
  const hash = sha256("unreachable");

  await assert.rejects(
    () => blobs.download("http://127.0.0.1:1/nope", hash, { maxSize: 1024, timeout: 2000 }),
    (error: BlobDownloadError) => {
      assert.equal(error.name, "BlobDownloadError");
      assert.equal(error.reason, "network");
      return true;
    },
  );
});

test("refuses to fetch anything that is not a public address", async () => {
  const blobs = await store(isPublicUrl);
  const hash = sha256("internal");
  const server = serve(() => new Response("internal"));

  try {
    for (const url of [`${server.url}/${hash}`, `http://169.254.169.254/${hash}`, `http://[::1]/${hash}`]) {
      await assert.rejects(
        () => blobs.download(url, hash, { maxSize: 1024 }),
        (error: BlobDownloadError) => {
          assert.equal(error.reason, "blocked");
          return true;
        },
        url,
      );
    }

    assert.equal(await blobs.has(hash), false);
  } finally {
    await server.close();
  }
});

test("refuses non-http schemes", async () => {
  const blobs = await store(isPublicUrl);
  const hash = sha256("file");

  await assert.rejects(
    () => blobs.download("file:///etc/passwd", hash, { maxSize: 1024 }),
    (error: BlobDownloadError) => {
      assert.equal(error.reason, "blocked");
      return true;
    },
  );
});

test("a redirect cannot smuggle a download past the address check", async () => {
  const hash = sha256("redirected");

  const target = serve(() => new Response("redirected"));
  const redirector = serve(() => new Response(null, { status: 302, headers: { location: `${target.url}/${hash}` } }));
  const targetPort = new URL(target.url).port;

  try {
    // The first hop passes the check, so only re-checking the redirect target
    // can catch this one.
    const blobs = await store(async (url) => url.port !== targetPort);
    await assert.rejects(
      () => blobs.download(`${redirector.url}/${hash}`, hash, { maxSize: 1024 }),
      (error: BlobDownloadError) => {
        assert.equal(error.reason, "blocked");
        return true;
      },
    );
    assert.equal(await blobs.has(hash), false);

    // When the target passes too, the same redirect is followed normally.
    const permissive = await store();
    assert.equal(await permissive.download(`${redirector.url}/${hash}`, hash, { maxSize: 1024 }), 10);
    assert.equal(await text(permissive.read(hash)), "redirected");
  } finally {
    await redirector.close();
    await target.close();
  }
});

test("gives up on a redirect loop", async () => {
  const blobs = await store();
  const hash = sha256("loop");

  const server = serve((request) => new Response(null, { status: 302, headers: { location: request.url } }));

  try {
    await assert.rejects(
      () => blobs.download(`${server.url}/${hash}`, hash, { maxSize: 1024 }),
      (error: BlobDownloadError) => {
        assert.match(error.message, /redirects/);
        return true;
      },
    );
  } finally {
    await server.close();
  }
});

test("lists and deletes stored blobs", async () => {
  const blobs = await store();
  const first = sha256("first");
  const second = sha256("second");
  await blobs.put(first, new TextEncoder().encode("first"));
  await blobs.put(second, new TextEncoder().encode("second"));

  const listed: string[] = [];
  for await (const hash of blobs.list()) listed.push(hash);
  assert.deepEqual(listed.sort(), [first, second].sort());

  assert.equal(await blobs.delete(first), true);
  assert.equal(await blobs.delete(first), false);
  assert.equal(await blobs.has(first), false);
  assert.equal(await blobs.has(second), true);
});

test("rejects paths that are not sha256 hashes", async () => {
  const blobs = await store();
  assert.throws(() => blobs.path("../../etc/passwd"), /Not a sha256/);
  assert.throws(() => blobs.path(""), /Not a sha256/);
});
