import assert from "node:assert/strict";
import { describe, it } from "node:test";
import { allows, formatPermission, parsePermission, parsePermissions } from "./permissions.ts";

describe("parsePermission", () => {
  it("reads a bare method", () => {
    assert.deepEqual(parsePermission("nip44_decrypt"), { method: "nip44_decrypt", kind: null });
  });

  it("reads a kind-scoped signing grant", () => {
    assert.deepEqual(parsePermission("sign_event:1"), { method: "sign_event", kind: 1 });
    assert.deepEqual(parsePermission("sign_event:0"), { method: "sign_event", kind: 0 });
  });

  it("normalizes whitespace and case", () => {
    assert.deepEqual(parsePermission("  SIGN_EVENT:30023 "), { method: "sign_event", kind: 30023 });
  });

  it("rejects a method nobody implements", () => {
    assert.throws(() => parsePermission("sign_psbt"), /Unknown permission/);
  });

  it("explains that implicit methods need no grant", () => {
    assert.throws(() => parsePermission("get_public_key"), /may call get_public_key/);
  });

  it("rejects a kind on a method that can never match one", () => {
    assert.throws(() => parsePermission("nip44_decrypt:4"), /never match/);
  });

  it("rejects a kind that isn't one", () => {
    for (const bad of ["sign_event:-1", "sign_event:1.5", "sign_event:abc", "sign_event:70000"]) {
      assert.throws(() => parsePermission(bad), /not a valid event kind/);
    }
  });
});

describe("parsePermissions", () => {
  it("drops duplicates", () => {
    assert.deepEqual(parsePermissions(["sign_event:1", "sign_event:1"]).map(formatPermission), ["sign_event:1"]);
  });

  it("drops a kind grant that a blanket grant beside it already covers", () => {
    const granted = parsePermissions(["sign_event:1", "sign_event", "nip44_decrypt"]);

    assert.deepEqual(granted.map(formatPermission), ["nip44_decrypt", "sign_event"]);
  });

  it("keeps kind grants for different methods apart", () => {
    const granted = parsePermissions(["sign_event:1", "nip04_encrypt"]);

    assert.deepEqual(granted.map(formatPermission), ["nip04_encrypt", "sign_event:1"]);
  });
});

describe("allows", () => {
  const granted = parsePermissions(["sign_event:1", "sign_event:30023", "nip44_decrypt"]);

  it("allows the methods every connection may call", () => {
    for (const method of ["connect", "ping", "get_public_key", "switch_relays", "logout"]) {
      assert.equal(allows([], method), true);
    }
  });

  it("allows a granted kind", () => {
    assert.equal(allows(granted, "sign_event", 1), true);
    assert.equal(allows(granted, "sign_event", 30023), true);
  });

  it("refuses a kind nobody granted", () => {
    assert.equal(allows(granted, "sign_event", 4), false);
  });

  it("refuses signing with no kind at all when only kinds were granted", () => {
    assert.equal(allows(granted, "sign_event"), false);
  });

  it("allows every kind under a blanket signing grant", () => {
    const blanket = parsePermissions(["sign_event"]);

    assert.equal(allows(blanket, "sign_event", 4), true);
    assert.equal(allows(blanket, "sign_event"), true);
  });

  it("allows a granted method that takes no kind", () => {
    assert.equal(allows(granted, "nip44_decrypt"), true);
    assert.equal(allows(granted, "nip44_encrypt"), false);
  });

  it("refuses a method it has never heard of", () => {
    assert.equal(allows(granted, "sign_psbt"), false);
  });
});
