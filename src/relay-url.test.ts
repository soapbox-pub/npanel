import assert from "node:assert/strict";
import { test } from "node:test";
import { parseRelayUrl } from "./relay-url.ts";

test("a relay origin keeps no trailing slash", () => {
  assert.equal(parseRelayUrl("wss://relay.example.com"), "wss://relay.example.com");
  assert.equal(parseRelayUrl("wss://relay.example.com/"), "wss://relay.example.com");
  assert.equal(parseRelayUrl("  ws://localhost:7777  "), "ws://localhost:7777");
});

test("a path is part of the relay and is kept", () => {
  assert.equal(parseRelayUrl("wss://example.com/relay"), "wss://example.com/relay");
});

test("only websocket schemes are relays", () => {
  assert.throws(() => parseRelayUrl("https://relay.example.com"), /wss:\/\/ or ws:\/\//);
  assert.throws(() => parseRelayUrl("relay.example.com"), /valid relay URL/);
  assert.throws(() => parseRelayUrl(""), /valid relay URL/);
});
