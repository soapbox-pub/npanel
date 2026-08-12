import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { type Server, type ServerResponse, createServer } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { text } from "node:stream/consumers";
import { after, test } from "node:test";
import { type BlobDownloadError, BlobStore } from "./blobs.ts";

const dirs: string[] = [];

async function store(): Promise<BlobStore> {
  const dir = await mkdtemp(join(tmpdir(), "npanel-blobs-"));
  dirs.push(dir);
  return new BlobStore(dir);
}

after(async () => {
  for (const dir of dirs) await rm(dir, { recursive: true, force: true });
});

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

/** Serve fixed responses on a random port; returns the base URL. */
async function serve(handler: (url: string, res: ServerResponse) => void) {
  const server: Server = createServer((req, res) => handler(req.url ?? "/", res));
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (typeof address === "string" || !address) throw new Error("no address");
  return {
    url: `http://127.0.0.1:${address.port}`,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
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
  const server = await serve((_url, res) => res.end("downloaded"));

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
  const server = await serve((_url, res) => res.end("something else"));

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
  const server = await serve((_url, res) => {
    res.statusCode = 404;
    res.end("nope");
  });

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

  const declared = await serve((_url, res) => {
    res.setHeader("content-length", String(body.length));
    res.end(body);
  });
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
  const chunked = await serve((_url, res) => {
    res.setHeader("transfer-encoding", "chunked");
    res.write(body.slice(0, 2048));
    res.end(body.slice(2048));
  });
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
