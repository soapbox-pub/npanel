// The dashboard's twin of the server's `src/identifiers.rs`: the same rules, so what
// the dashboard shows is what the server will do. Change both together.

import { nip19 } from "nostr-tools";

/**
 * Accept an npub, nprofile, or hex pubkey; always return hex.
 *
 * Shared with the dashboard, so the author a form previews while you type is
 * the one the server will store.
 */
export function parsePubkey(input: string): string {
  const value = input.trim().replace(/^nostr:/i, "");

  if (/^[0-9a-fA-F]{64}$/.test(value)) return value.toLowerCase();

  try {
    const decoded = nip19.decode(value);
    if (decoded.type === "npub") return decoded.data;
    if (decoded.type === "nprofile") return decoded.data.pubkey;
  } catch {
    // Fall through to the error below.
  }

  throw new Error(`Not a valid pubkey: ${input.trim()}`);
}
