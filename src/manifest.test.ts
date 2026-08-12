import assert from "node:assert/strict";
import { test } from "node:test";
import type { NostrEvent } from "@nostrify/nostrify";
import {
  getManifestPaths,
  getManifestRelays,
  getManifestServers,
  isSiteManifest,
  manifestAddress,
  parseRelayList,
  parseServerList,
  resolveManifestPath,
} from "./manifest.ts";
import { NAMED_SITE_KIND, ROOT_SITE_KIND } from "./nsite-address.ts";

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const HASH_A = "a".repeat(64);
const HASH_B = "b".repeat(64);
const HASH_C = "c".repeat(64);

function event(partial: Partial<NostrEvent>): NostrEvent {
  return {
    id: "0".repeat(64),
    pubkey: PUBKEY,
    created_at: 1,
    kind: NAMED_SITE_KIND,
    tags: [],
    content: "",
    sig: "0".repeat(128),
    ...partial,
  };
}

test("identifies site manifests", () => {
  assert.equal(isSiteManifest(event({ kind: ROOT_SITE_KIND })), true);
  assert.equal(isSiteManifest(event({ kind: NAMED_SITE_KIND })), true);
  assert.equal(isSiteManifest(event({ kind: 1 })), false);
});

test("derives the address from a manifest", () => {
  assert.deepEqual(manifestAddress(event({ kind: NAMED_SITE_KIND, tags: [["d", "docs"]] })), {
    pubkey: PUBKEY,
    kind: NAMED_SITE_KIND,
    identifier: "docs",
  });
  assert.deepEqual(manifestAddress(event({ kind: ROOT_SITE_KIND, tags: [["d", "ignored"]] })), {
    pubkey: PUBKEY,
    kind: ROOT_SITE_KIND,
    identifier: "",
  });
});

test("builds the path map, normalizing paths and skipping bad hashes", () => {
  const paths = getManifestPaths(
    event({
      tags: [
        ["path", "index.html", HASH_A],
        ["path", "/blog/post.html", HASH_B.toUpperCase()],
        ["path", "/broken.html", "not-a-hash"],
        ["path", "/missing-hash.html"],
        ["other", "/ignored.html", HASH_C],
      ],
    }),
  );

  assert.equal(paths.get("/index.html"), HASH_A);
  assert.equal(paths.get("/blog/post.html"), HASH_B);
  assert.equal(paths.has("/broken.html"), false);
  assert.equal(paths.has("/missing-hash.html"), false);
  assert.equal(paths.has("/ignored.html"), false);
  assert.equal(paths.size, 2);
});

test("resolves exact paths, index fallbacks, and 404s", () => {
  const paths = new Map([
    ["/index.html", HASH_A],
    ["/blog/index.html", HASH_B],
    ["/404.html", HASH_C],
  ]);

  assert.deepEqual(resolveManifestPath(paths, "/index.html"), {
    path: "/index.html",
    sha256: HASH_A,
    is404: false,
  });
  assert.deepEqual(resolveManifestPath(paths, "/"), {
    path: "/index.html",
    sha256: HASH_A,
    is404: false,
  });
  assert.deepEqual(resolveManifestPath(paths, "/blog"), {
    path: "/blog/index.html",
    sha256: HASH_B,
    is404: false,
  });
  assert.deepEqual(resolveManifestPath(paths, "/nope.html"), {
    path: "/404.html",
    sha256: HASH_C,
    is404: true,
  });
});

test("resolution normalizes traversal and percent-encoding", () => {
  const paths = new Map([
    ["/index.html", HASH_A],
    ["/my file.txt", HASH_B],
  ]);

  assert.equal(resolveManifestPath(paths, "/../../index.html")?.sha256, HASH_A);
  assert.equal(resolveManifestPath(paths, "/blog/../index.html")?.sha256, HASH_A);
  assert.equal(resolveManifestPath(paths, "/my%20file.txt")?.sha256, HASH_B);
  assert.equal(resolveManifestPath(paths, "/bad%zz"), undefined);
});

test("returns undefined when there is no match and no 404 page", () => {
  assert.equal(resolveManifestPath(new Map([["/index.html", HASH_A]]), "/nope.html"), undefined);
});

test("reads relay and server hints", () => {
  const manifest = event({
    tags: [
      ["r", "wss://relay.example.com"],
      ["relay", "wss://other.example.com"],
      ["server", "https://blossom.example.com"],
      ["server", ""],
    ],
  });

  assert.deepEqual(getManifestRelays(manifest), ["wss://relay.example.com", "wss://other.example.com"]);
  assert.deepEqual(getManifestServers(manifest), ["https://blossom.example.com"]);
});

test("parses kind 10002 relay lists with markers", () => {
  const list = event({
    kind: 10002,
    tags: [
      ["r", "wss://both.example.com"],
      ["r", "wss://read.example.com", "read"],
      ["r", "wss://write.example.com", "write"],
    ],
  });

  assert.deepEqual(parseRelayList(list), {
    read: ["wss://both.example.com", "wss://read.example.com"],
    write: ["wss://both.example.com", "wss://write.example.com"],
  });
});

test("parses kind 10063 server lists", () => {
  const list = event({
    kind: 10063,
    tags: [
      ["server", "https://a.example.com"],
      ["server", "https://b.example.com"],
    ],
  });

  assert.deepEqual(parseServerList(list), ["https://a.example.com", "https://b.example.com"]);
});
