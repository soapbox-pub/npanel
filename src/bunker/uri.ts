/**
 * The two connection strings NIP-46 defines, which run in opposite directions.
 *
 * `bunker://` is signer-initiated: the signer mints it and a client consumes
 * it. `nostrconnect://` is client-initiated: the client publishes it and the
 * signer consumes it. npanel is on both sides — a client to the phone that
 * really holds the key, and a signer to whoever it mints connections for — so
 * it builds and reads both.
 *
 * Nostrify parses `bunker://` (`BunkerURI`) but has nothing for
 * `nostrconnect://`, so that half lives here.
 */

export interface NostrConnectParams {
  /** The client's own pubkey, which is the URI's authority. */
  clientPubkey: string;
  relays: string[];
  /** Echoed back by the signer as the `connect` result, proving it answered. */
  secret: string;
  /** NIP-46 permission specs the client would like granted. */
  perms?: string[];
  name?: string;
  url?: string;
}

/** Build the URI a signer scans or pastes to connect back to a client. */
export function formatNostrConnectUri(params: NostrConnectParams): string {
  const query = new URLSearchParams();

  // Repeated rather than comma-joined, per NIP-46, and first so the relays
  // stay readable in a URI that a person may have to eyeball.
  for (const relay of params.relays) {
    query.append("relay", relay);
  }

  query.set("secret", params.secret);
  if (params.perms?.length) query.set("perms", params.perms.join(","));
  if (params.name) query.set("name", params.name);
  if (params.url) query.set("url", params.url);

  return `nostrconnect://${params.clientPubkey}?${query.toString()}`;
}
