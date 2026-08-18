import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { describe, it } from "node:test";
import { Secrets } from "./secrets.ts";

const KEY = "a".repeat(64);
const OTHER_KEY = "b".repeat(64);

describe("Secrets", () => {
  it("round trips a credential", () => {
    const secrets = Secrets.fromHex(KEY);
    const plaintext = randomBytes(32).toString("hex");

    assert.equal(secrets.decrypt(secrets.encrypt(plaintext)), plaintext);
  });

  it("does not produce the same ciphertext twice", () => {
    const secrets = Secrets.fromHex(KEY);

    assert.notEqual(secrets.encrypt("nsec"), secrets.encrypt("nsec"));
  });

  it("refuses a payload sealed with another key", () => {
    const payload = Secrets.fromHex(KEY).encrypt("nsec");

    assert.throws(() => Secrets.fromHex(OTHER_KEY).decrypt(payload), /SECRET_KEY may have changed/);
  });

  it("refuses a tampered payload", () => {
    const secrets = Secrets.fromHex(KEY);
    const payload = secrets.encrypt("nsec");
    const tampered = `${payload.slice(0, -4)}AAAA`;

    assert.throws(() => secrets.decrypt(tampered), /Could not decrypt/);
  });

  it("names a key without revealing it", () => {
    const fingerprint = Secrets.fromHex(KEY).fingerprint;

    assert.equal(fingerprint, Secrets.fromHex(KEY).fingerprint);
    assert.notEqual(fingerprint, Secrets.fromHex(OTHER_KEY).fingerprint);
    assert.ok(!KEY.includes(fingerprint));
  });

  it("rejects a key that isn't 32 bytes of hex", () => {
    for (const bad of ["", "abc", "z".repeat(64), "a".repeat(63), "a".repeat(66)]) {
      assert.throws(() => Secrets.fromHex(bad), /64 hex characters/);
    }
  });

  it("accepts a key however it was pasted", () => {
    const expected = Secrets.fromHex(KEY).fingerprint;

    assert.equal(Secrets.fromHex(`  ${KEY.toUpperCase()}  `).fingerprint, expected);
  });
});
