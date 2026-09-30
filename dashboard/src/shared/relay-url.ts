// The dashboard's twin of the server's `src/identifiers.rs`: the same rules, so what
// the dashboard shows is what the server will do. Change both together.

/**
 * Relay URLs, normalized the one way npanel stores them.
 *
 * Shared by the settings schema, the NIP-05 names API, and the dashboard forms
 * for both — a relay someone types into one of those places has to be the same
 * string as the identical relay typed into another, or the same relay stored
 * twice reads as two.
 */

/**
 * Accept a `wss://` URL; always return it normalized.
 *
 * Plain `ws://` is refused everywhere. Anything on the path can read or rewrite
 * an unencrypted relay connection — which manifests npanel sees, which relays a
 * NIP-05 name points clients at — and no relay worth naming lacks TLS.
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

  if (url.protocol !== "wss:") {
    throw new Error(`A relay URL has to start with wss://: ${value}`);
  }

  return url.toString().replace(/\/$/, "");
}
