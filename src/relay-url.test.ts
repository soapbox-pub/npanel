import assert from "node:assert/strict";
import { test } from "node:test";
import { parseRelayUrl } from "./relay-url.ts";

test("a relay origin keeps no trailing slash", () => {
  assert.equal(parseRelayUrl("wss://relay.example.com"), "wss://relay.example.com");
  assert.equal(parseRelayUrl("wss://relay.example.com/"), "wss://relay.example.com");
  assert.equal(parseRelayUrl("  wss://localhost:7777  "), "wss://localhost:7777");
});

test("a path is part of the relay and is kept", () => {
  assert.equal(parseRelayUrl("wss://example.com/relay"), "wss://example.com/relay");
});

test("only wss:// is a relay", () => {
  assert.throws(() => parseRelayUrl("ws://relay.example.com"), /start with wss:\/\//);
  assert.throws(() => parseRelayUrl("https://relay.example.com"), /start with wss:\/\//);
  assert.throws(() => parseRelayUrl("relay.example.com"), /valid relay URL/);
  assert.throws(() => parseRelayUrl(""), /valid relay URL/);
});
