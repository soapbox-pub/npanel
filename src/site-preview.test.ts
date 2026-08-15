import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, test } from "node:test";
import { BlobStore } from "./blobs.ts";
import { SitePreviews, extractMeta, resolveImage } from "./site-preview.ts";

const dirs: string[] = [];

after(async () => {
  for (const dir of dirs) await rm(dir, { recursive: true, force: true });
});

async function store(): Promise<BlobStore> {
  const dir = await mkdtemp(join(tmpdir(), "npanel-preview-"));
  dirs.push(dir);
  return new BlobStore(dir);
}

async function put(blobs: BlobStore, content: string): Promise<string> {
  const sha256 = createHash("sha256").update(content).digest("hex");
  await blobs.put(sha256, new TextEncoder().encode(content));
  return sha256;
}

const ORIGIN = new URL("https://docs.example.com/");

test("prefers Open Graph, then Twitter, then the document itself", async () => {
  const og = extractMeta(`
    <html><head>
      <title>Document title</title>
      <meta name="description" content="Plain description">
      <meta property="og:title" content="Open Graph title">
      <meta property="og:description" content="Open Graph description">
      <meta name="twitter:title" content="Twitter title">
    </head></html>
  `);
  assert.deepEqual(og, { title: "Open Graph title", description: "Open Graph description" });

  const twitter = extractMeta(`
    <html><head>
      <title>Document title</title>
      <meta name="twitter:title" content="Twitter title">
      <meta name="twitter:description" content="Twitter description">
    </head></html>
  `);
  assert.deepEqual(twitter, { title: "Twitter title", description: "Twitter description" });

  const plain = extractMeta(
    '<html><head><title>Document title</title><meta name="description" content="Plain"></head></html>',
  );
  assert.deepEqual(plain, { title: "Document title", description: "Plain" });
});

test("reads a document that says nothing about itself", () => {
  assert.deepEqual(extractMeta("<html><body><h1>Hello</h1></body></html>"), {});
  assert.deepEqual(extractMeta(""), {});
  assert.deepEqual(extractMeta("not html at all"), {});
});

test("decodes entities and collapses whitespace", () => {
  const meta = extractMeta(`
    <html><head>
      <meta property="og:title" content="Alex&#39;s notes &amp; drafts">
      <meta property="og:description" content="One   line
      split across two">
    </head></html>
  `);

  assert.equal(meta.title, "Alex's notes & drafts");
  assert.equal(meta.description, "One line split across two");
});

test("keeps the first tag of a kind, as og:image repeats for its sizes", () => {
  const meta = extractMeta(`
    <html><head>
      <meta property="og:image" content="/first.png">
      <meta property="og:image" content="/second.png">
    </head></html>
  `);

  assert.equal(meta.image, "/first.png");
});

test("a site cannot put an essay on someone else's homepage", () => {
  const meta = extractMeta(`
    <html><head>
      <meta property="og:title" content="${"word ".repeat(200)}">
      <meta property="og:description" content="${"x".repeat(5000)}">
    </head></html>
  `);

  const { title = "", description = "" } = meta;
  assert.ok(title.length <= 201, `title was ${title.length}`);
  assert.ok(description.length <= 301, `description was ${description.length}`);
  assert.ok(title.endsWith("…"));
});

test("resolves images against the site that named them", () => {
  assert.equal(resolveImage("/og.png", ORIGIN), "https://docs.example.com/og.png");
  assert.equal(resolveImage("og.png", ORIGIN), "https://docs.example.com/og.png");
  assert.equal(resolveImage("https://cdn.example.net/og.png", ORIGIN), "https://cdn.example.net/og.png");
  assert.equal(resolveImage(undefined, ORIGIN), null);

  // A homepage visit must not become a plaintext request somewhere else, and
  // the dashboard's own CSP would refuse these anyway.
  assert.equal(resolveImage("http://insecure.example.net/og.png", ORIGIN), null);
  assert.equal(resolveImage("data:image/png;base64,AAAA", ORIGIN), null);
  assert.equal(resolveImage("javascript:alert(1)", ORIGIN), null);
});

test("previews a stored document, and remembers it by hash", async () => {
  const blobs = await store();
  const previews = new SitePreviews(blobs);

  const sha256 = await put(
    blobs,
    `<html><head>
      <title>Docs</title>
      <meta property="og:description" content="How it works">
      <meta property="og:image" content="/cover.png">
    </head></html>`,
  );

  assert.deepEqual(await previews.get(sha256, ORIGIN), {
    title: "Docs",
    description: "How it works",
    image: "https://docs.example.com/cover.png",
  });

  // The same document on another host resolves to that host, from one parse.
  assert.equal(
    (await previews.get(sha256, new URL("https://blog.example.org/"))).image,
    "https://blog.example.org/cover.png",
  );

  await rm(blobs.path(sha256));
  assert.equal((await previews.get(sha256, ORIGIN)).title, "Docs");
});

test("a missing document previews as nothing at all", async () => {
  const previews = new SitePreviews(await store());

  assert.deepEqual(await previews.get("a".repeat(64), ORIGIN), {
    title: null,
    description: null,
    image: null,
  });
});

test("stops reading a document that never gets to the point", async () => {
  const blobs = await store();
  const previews = new SitePreviews(blobs);

  // The tags that matter are in <head>; a site is free to inline a novel
  // below it, and npanel is free to stop reading.
  const sha256 = await put(
    blobs,
    `<html><head><title>Big</title></head><body>${"<p>padding</p>".repeat(40_000)}</body></html>`,
  );

  const preview = await previews.get(sha256, ORIGIN);
  assert.equal(preview.title, "Big");
});
