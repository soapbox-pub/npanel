/**
 * Relay URLs, normalized the one way npanel stores them.
 *
 * Shared by the settings schema, the NIP-05 names API, and the dashboard forms
 * for both — a relay someone types into one of those places has to be the same
 * string as the identical relay typed into another, or the same relay stored
 * twice reads as two.
 */

/**
 * Accept a `ws://` or `wss://` URL; always return it normalized.
 *
 * The trailing slash `URL` adds to a bare origin is dropped, because a relay is
 * named by its origin and `wss://relay.example.com/` is the same relay as
 * `wss://relay.example.com`. A path that isn't just `/` is kept: relays do get
 * published under one.
 */
export function parseRelayUrl(input: string): string {
  const value = input.trim();

  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new Error(`Not a valid relay URL: ${value}`);
  }

  if (url.protocol !== "wss:" && url.protocol !== "ws:") {
    throw new Error(`A relay URL has to start with wss:// or ws://: ${value}`);
  }

  return url.toString().replace(/\/$/, "");
}
