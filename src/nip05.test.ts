import assert from "node:assert/strict";
import { test } from "node:test";
import { nip05Document, parseNip05Name, parseRelayList } from "./nip05.ts";

const ALICE = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const BOB = "b0635d6a9851d3aed0cd6c495b282167acf761729078d975fc341b22650b07b9";

test("names are lowercased and trimmed", () => {
  assert.equal(parseNip05Name("  Bob "), "bob");
  assert.equal(parseNip05Name("_"), "_");
  assert.equal(parseNip05Name("first.last_1-2"), "first.last_1-2");
});

test("names outside the NIP's local-part rule are refused", () => {
  assert.throws(() => parseNip05Name(""), /Enter a name/);
  assert.throws(() => parseNip05Name("   "), /Enter a name/);
  assert.throws(() => parseNip05Name("bob@example.com"), /letters, digits/);
  assert.throws(() => parseNip05Name("bo b"), /letters, digits/);
  assert.throws(() => parseNip05Name("bób"), /letters, digits/);
  assert.throws(() => parseNip05Name("b".repeat(65)), /at most 64/);
});

test("a document maps names to pubkeys", () => {
  const document = nip05Document([
    { name: "alice", pubkey: ALICE, relays: [] },
    { name: "bob", pubkey: BOB, relays: ["wss://relay.example.com"] },
  ]);

  assert.deepEqual(document.names, { alice: ALICE, bob: BOB });
  assert.deepEqual(document.relays, { [BOB]: ["wss://relay.example.com"] });
});

test("relays is left out when nobody named one", () => {
  const document = nip05Document([{ name: "alice", pubkey: ALICE, relays: [] }]);

  assert.deepEqual(document, { names: { alice: ALICE } });
  assert.equal("relays" in document, false);
});

test("two names for one pubkey union their relays", () => {
  const document = nip05Document([
    { name: "alice", pubkey: ALICE, relays: ["wss://one.example.com", "wss://two.example.com"] },
    { name: "_", pubkey: ALICE, relays: ["wss://two.example.com", "wss://three.example.com"] },
  ]);

  assert.deepEqual(document.relays, {
    [ALICE]: ["wss://one.example.com", "wss://two.example.com", "wss://three.example.com"],
  });
});

test("a stored relay list that isn't one reads as empty", () => {
  assert.deepEqual(parseRelayList('["wss://relay.example.com"]'), ["wss://relay.example.com"]);
  assert.deepEqual(parseRelayList("[]"), []);
  assert.deepEqual(parseRelayList("not json"), []);
  assert.deepEqual(parseRelayList('{"a":1}'), []);
  assert.deepEqual(parseRelayList('["wss://ok.example.com", 7]'), ["wss://ok.example.com"]);
});
