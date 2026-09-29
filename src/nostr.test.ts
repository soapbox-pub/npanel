import assert from "node:assert/strict";
import { test } from "node:test";
import { isPublicAddress } from "./net.ts";
import { MAX_AUTHOR_RELAYS, vetRelayHints } from "./nostr.ts";

/** The real address rule, over literals only, so the tests never touch DNS. */
const literalIsPublic = async (url: URL) => isPublicAddress(url.hostname.replace(/^\[|\]$/g, ""));

test("an author's relays on private addresses are never connected to", async () => {
  const relays = await vetRelayHints(
    [
      "wss://127.0.0.1:9051",
      "wss://169.254.169.254/latest",
      "wss://10.0.0.5",
      "wss://[::1]:7777",
      "wss://93.184.215.14",
      "wss://[2606:4700::1111]",
    ],
    literalIsPublic,
  );

  assert.deepEqual(relays, ["wss://93.184.215.14", "wss://[2606:4700::1111]"]);
});

test("entries that are not wss:// URLs are dropped, and duplicates kept once", async () => {
  const relays = await vetRelayHints(
    [
      "https://relay.example.com",
      "ws://plain.example.com",
      "not a url",
      "wss://relay.example.com/",
      "wss://relay.example.com",
    ],
    async () => true,
  );

  assert.deepEqual(relays, ["wss://relay.example.com"]);
});

test("a long list is cut to the first few relays that pass", async () => {
  const urls = Array.from({ length: 50 }, (_, index) => `wss://relay${index}.example.com`);
  const checked: string[] = [];

  const relays = await vetRelayHints(urls, async (url) => {
    checked.push(url.hostname);
    return true;
  });

  assert.deepEqual(relays, urls.slice(0, MAX_AUTHOR_RELAYS));
  assert.ok(checked.length < urls.length, "only the head of the list is resolved");
});

test("a relay whose name fails to resolve is dropped rather than failing the list", async () => {
  const relays = await vetRelayHints(["wss://gone.example.com", "wss://relay.example.com"], async (url) => {
    if (url.hostname === "gone.example.com") throw new Error("ENOTFOUND");
    return true;
  });

  assert.deepEqual(relays, ["wss://relay.example.com"]);
});
