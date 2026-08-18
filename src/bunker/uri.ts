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

/** Read the URI a client published, or throw a message worth showing. */
export function parseNostrConnectUri(input: string): NostrConnectParams {
  const value = input.trim();
  if (!value.startsWith("nostrconnect://")) {
    throw new Error("That isn't a nostrconnect:// URI.");
  }

  // `new URL` mangles a non-special scheme's authority differently on
  // different runtimes, and the pubkey is fixed-width hex anyway.
  const [clientPubkey, query = ""] = value.slice("nostrconnect://".length).split("?", 2);
  if (!/^[0-9a-f]{64}$/i.test(clientPubkey)) {
    throw new Error("That URI doesn't start with a client pubkey.");
  }

  const params = new URLSearchParams(query);
  const relays = params.getAll("relay").filter(Boolean);
  const secret = params.get("secret");

  if (!relays.length) {
    throw new Error("That URI names no relays, so there is nowhere to answer it.");
  }
  // NIP-46 requires it, and without one the client can't tell our answer from
  // an impostor's — which is the only thing stopping connection spoofing here.
  if (!secret) {
    throw new Error("That URI carries no secret, so the client couldn't verify the answer.");
  }

  const perms = params.get("perms");

  return {
    clientPubkey: clientPubkey.toLowerCase(),
    relays,
    secret,
    perms: perms
      ? perms
          .split(",")
          .map((spec) => spec.trim())
          .filter(Boolean)
      : undefined,
    name: params.get("name") ?? undefined,
    url: params.get("url") ?? undefined,
  };
}

export interface BunkerUriParams {
  /** The pubkey a client addresses its requests to. */
  signerPubkey: string;
  relays: string[];
  /** Single-use claim token, spent by the first client that connects. */
  secret: string;
}

/** Build the URI a client pastes to connect to a connection npanel minted. */
export function formatBunkerUri(params: BunkerUriParams): string {
  const query = new URLSearchParams();

  for (const relay of params.relays) {
    query.append("relay", relay);
  }
  query.set("secret", params.secret);

  return `bunker://${params.signerPubkey}?${query.toString()}`;
}
