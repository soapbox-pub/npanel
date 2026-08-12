import assert from "node:assert/strict";
import { test } from "node:test";
import { nip19 } from "nostr-tools";
import {
  NAMED_SITE_KIND,
  ROOT_SITE_KIND,
  decodePubkeyB36,
  encodeNaddr,
  encodePubkeyB36,
  formatNsiteAddress,
  formatSubdomainLabel,
  parseNsiteAddress,
} from "./nsite-address.ts";

const PUBKEY = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
const NPUB = nip19.npubEncode(PUBKEY);

test("parses a hex pubkey as a root site", () => {
  assert.deepEqual(parseNsiteAddress(PUBKEY), {
    pubkey: PUBKEY,
    kind: ROOT_SITE_KIND,
    identifier: "",
  });
  assert.deepEqual(parseNsiteAddress(PUBKEY.toUpperCase()).pubkey, PUBKEY);
});

test("parses npub, nprofile, and nostr: prefixes", () => {
  assert.deepEqual(parseNsiteAddress(NPUB), {
    pubkey: PUBKEY,
    kind: ROOT_SITE_KIND,
    identifier: "",
  });
  assert.deepEqual(parseNsiteAddress(`nostr:${NPUB}`).pubkey, PUBKEY);
  assert.deepEqual(parseNsiteAddress(`  ${NPUB}  `).pubkey, PUBKEY);

  const nprofile = nip19.nprofileEncode({ pubkey: PUBKEY, relays: ["wss://relay.example.com"] });
  assert.deepEqual(parseNsiteAddress(nprofile), {
    pubkey: PUBKEY,
    kind: ROOT_SITE_KIND,
    identifier: "",
  });
});

test("parses naddr for both site kinds", () => {
  const named = nip19.naddrEncode({ pubkey: PUBKEY, kind: NAMED_SITE_KIND, identifier: "docs" });
  assert.deepEqual(parseNsiteAddress(named), {
    pubkey: PUBKEY,
    kind: NAMED_SITE_KIND,
    identifier: "docs",
  });

  const root = nip19.naddrEncode({ pubkey: PUBKEY, kind: ROOT_SITE_KIND, identifier: "" });
  assert.deepEqual(parseNsiteAddress(root), {
    pubkey: PUBKEY,
    kind: ROOT_SITE_KIND,
    identifier: "",
  });
});

test("rejects naddr for non-site kinds", () => {
  const article = nip19.naddrEncode({ pubkey: PUBKEY, kind: 30023, identifier: "post" });
  assert.throws(() => parseNsiteAddress(article), /not an nsite/);
});

test("parses a tag values", () => {
  assert.deepEqual(parseNsiteAddress(`${NAMED_SITE_KIND}:${PUBKEY}:docs`), {
    pubkey: PUBKEY,
    kind: NAMED_SITE_KIND,
    identifier: "docs",
  });
  assert.deepEqual(parseNsiteAddress(`${ROOT_SITE_KIND}:${PUBKEY}:`), {
    pubkey: PUBKEY,
    kind: ROOT_SITE_KIND,
    identifier: "",
  });
  assert.throws(() => parseNsiteAddress(`${NAMED_SITE_KIND}:${PUBKEY}:`), /requires an identifier/);
  assert.throws(() => parseNsiteAddress(`${ROOT_SITE_KIND}:${PUBKEY}:docs`), /cannot have an identifier/);
});

test("parses NIP-5A subdomain labels", () => {
  const label = `${encodePubkeyB36(PUBKEY)}docs`;
  assert.deepEqual(parseNsiteAddress(label), {
    pubkey: PUBKEY,
    kind: NAMED_SITE_KIND,
    identifier: "docs",
  });
  assert.deepEqual(parseNsiteAddress(`${label}.nsite.lol`), {
    pubkey: PUBKEY,
    kind: NAMED_SITE_KIND,
    identifier: "docs",
  });
});

test("parses full NIP-5A URLs", () => {
  assert.deepEqual(parseNsiteAddress(`https://${NPUB}.nsite.lol`), {
    pubkey: PUBKEY,
    kind: ROOT_SITE_KIND,
    identifier: "",
  });
  assert.deepEqual(parseNsiteAddress(`https://${NPUB}.nsite.lol/blog/post.html?x=1`), {
    pubkey: PUBKEY,
    kind: ROOT_SITE_KIND,
    identifier: "",
  });
  assert.deepEqual(parseNsiteAddress(`${encodePubkeyB36(PUBKEY)}docs.nsite.lol`), {
    pubkey: PUBKEY,
    kind: NAMED_SITE_KIND,
    identifier: "docs",
  });
});

test("rejects nsec loudly and other identifiers helpfully", () => {
  const nsec = nip19.nsecEncode(new Uint8Array(32).fill(7));
  assert.throws(() => parseNsiteAddress(nsec), /private key/);

  const note = nip19.noteEncode(PUBKEY);
  assert.throws(() => parseNsiteAddress(note), /does not point to a site/);
});

test("rejects empty and unrecognized input", () => {
  assert.throws(() => parseNsiteAddress("   "), /Enter an address/);
  assert.throws(() => parseNsiteAddress("hello world"), /Unrecognized address/);
  assert.throws(() => parseNsiteAddress("npub1notreallyvalid"), /not a valid Nostr identifier/i);
});

test("formats the canonical a tag and naddr", () => {
  const address = { pubkey: PUBKEY, kind: NAMED_SITE_KIND, identifier: "docs" };
  assert.equal(formatNsiteAddress(address), `${NAMED_SITE_KIND}:${PUBKEY}:docs`);
  assert.deepEqual(parseNsiteAddress(encodeNaddr(address)), address);
});

test("base36 pubkey encoding round-trips", () => {
  const encoded = encodePubkeyB36(PUBKEY);
  assert.equal(encoded?.length, 50);
  assert.equal(decodePubkeyB36(encoded ?? ""), PUBKEY);
  assert.equal(encodePubkeyB36("nothex"), undefined);
  assert.equal(decodePubkeyB36("zzz"), undefined);
});

test("formats subdomain labels, or nothing when the identifier is too long", () => {
  assert.equal(formatSubdomainLabel({ pubkey: PUBKEY, kind: ROOT_SITE_KIND, identifier: "" }), NPUB);
  assert.equal(
    formatSubdomainLabel({ pubkey: PUBKEY, kind: NAMED_SITE_KIND, identifier: "docs" }),
    `${encodePubkeyB36(PUBKEY)}docs`,
  );
  assert.equal(
    formatSubdomainLabel({ pubkey: PUBKEY, kind: NAMED_SITE_KIND, identifier: "an-identifier-far-too-long" }),
    undefined,
  );
});

test("a leading zero pubkey still encodes to a 50 character label", () => {
  const pubkey = `00${"a".repeat(62)}`;
  const label = encodePubkeyB36(pubkey);
  assert.equal(label?.length, 50);
  assert.equal(decodePubkeyB36(label ?? ""), pubkey);
});
