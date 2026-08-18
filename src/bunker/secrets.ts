import { createHash } from "node:crypto";
import { nip44 } from "nostr-tools";

/**
 * Symmetric encryption for the credentials npanel holds on someone's behalf.
 *
 * The payload format is NIP-44 v2 — XChaCha20-Poly1305 with per-message keys
 * derived from a random nonce, a version byte, and a MAC. It is reused here as
 * a general-purpose sealed-box format rather than for a conversation: NIP-44's
 * "conversation key" is only ever used as a 32-byte HKDF salt input, so
 * handing it `SECRET_KEY` directly is sound, and it means no new dependency
 * and no hand-rolled framing for a file whose whole job is not to be
 * hand-rolled.
 *
 * What this protects: a stolen database, a leaked backup, a misplaced disk.
 * What it does not protect: a compromise of the running server, which has the
 * key in its environment. That is worth saying plainly rather than implying
 * otherwise — see {@link file://../../docs/architecture.md}.
 */
export class Secrets {
  private constructor(private readonly key: Uint8Array) {}

  /** Build from a 64-character hex string, as `SECRET_KEY` supplies it. */
  static fromHex(hex: string): Secrets {
    const value = hex.trim().toLowerCase();
    if (!/^[0-9a-f]{64}$/.test(value)) {
      throw new Error("A secret key must be 64 hex characters. Generate one with: openssl rand -hex 32");
    }
    return new Secrets(Uint8Array.from(Buffer.from(value, "hex")));
  }

  /**
   * A stable, non-reversible name for this key.
   *
   * Stored the first time npanel encrypts anything and compared on every boot,
   * so starting with the wrong `SECRET_KEY` says so in one sentence instead of
   * failing later as an unexplained MAC error on the first signing request.
   * Domain-separated so the digest can't be reused as an oracle for anything
   * else derived from the same key.
   */
  get fingerprint(): string {
    return createHash("sha256").update("npanel/secret-key-fingerprint/v1").update(this.key).digest("hex").slice(0, 16);
  }

  encrypt(plaintext: string): string {
    return nip44.v2.encrypt(plaintext, this.key);
  }

  /** Throws when the payload was sealed with a different key, or was altered. */
  decrypt(payload: string): string {
    try {
      return nip44.v2.decrypt(payload, this.key);
    } catch {
      // The MAC is the only authentication there is, so a wrong key and a
      // tampered payload are indistinguishable. Say the likely thing.
      throw new Error("Could not decrypt a stored credential. SECRET_KEY may have changed.");
    }
  }
}
