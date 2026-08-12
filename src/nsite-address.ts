import { nip19 } from "nostr-tools";

/** Replaceable manifest event for a user's root site. */
export const ROOT_SITE_KIND = 15128;
/** Addressable manifest event for a named site, identified by its `d` tag. */
export const NAMED_SITE_KIND = 35128;

export const SITE_KINDS = [ROOT_SITE_KIND, NAMED_SITE_KIND];

/**
 * A site, decomposed into the only shape npanel stores.
 *
 * Every input format the dashboard accepts — npub, nprofile, naddr, `a` tag,
 * NIP-5A subdomain label, or a full NIP-5A URL — normalizes to this.
 */
export interface NsiteAddress {
  /** Hex pubkey of the author. */
  pubkey: string;
  kind: number;
  /** `d` tag of a named site; empty string for a root site. */
  identifier: string;
}

const HEX64 = /^[0-9a-f]{64}$/;
const A_TAG = /^(\d+):([0-9a-fA-F]{64}):(.*)$/;
const CANONICAL_PUBKEY_LENGTH = 50;
const MAX_PUBKEY = (1n << 256n) - 1n;

/** Identifiers that fit in a DNS label alongside the base36 pubkey. */
export const CANONICAL_SITE_IDENTIFIER = /^(?=.{1,13}$)[a-z0-9-]*[a-z0-9]$/;

/**
 * Parse any user-supplied site address into its canonical form.
 *
 * Throws an `Error` whose message is safe to show in the UI.
 */
export function parseNsiteAddress(input: string): NsiteAddress {
  const trimmed = input.trim();
  if (!trimmed) {
    throw new Error("Enter an address.");
  }

  let value = trimmed.replace(/^nostr:/i, "").replace(/^web\+nostr:/i, "");

  // Full URL, eg https://<label>.nsite.lol/some/page
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(value)) {
    let url: URL;
    try {
      url = new URL(value);
    } catch {
      throw new Error(`Not a valid URL: ${trimmed}`);
    }
    value = url.hostname;
  }

  // Hostname, eg <label>.nsite.lol — only the first label identifies the site.
  // `a` tag values also contain no dots, so this is unambiguous.
  if (value.includes(".") && !value.includes(":")) {
    value = value.split(".")[0];
  }

  value = value.trim();

  if (HEX64.test(value.toLowerCase())) {
    return { pubkey: value.toLowerCase(), kind: ROOT_SITE_KIND, identifier: "" };
  }

  const aTag = value.match(A_TAG);
  if (aTag) {
    return normalize({
      kind: Number.parseInt(aTag[1], 10),
      pubkey: aTag[2].toLowerCase(),
      identifier: aTag[3],
    });
  }

  if (/^(npub|nprofile|naddr|nsec|note|nevent)1/i.test(value)) {
    return fromNip19(value.toLowerCase(), trimmed);
  }

  const label = parseCanonicalSiteLabel(value.toLowerCase());
  if (label) return label;

  throw new Error(
    `Unrecognized address: ${trimmed}. Use an npub, naddr, ${NAMED_SITE_KIND}:<pubkey>:<identifier>, or an nsite URL.`,
  );
}

function fromNip19(value: string, original: string): NsiteAddress {
  let decoded: nip19.DecodedResult;
  try {
    decoded = nip19.decode(value);
  } catch {
    throw new Error(`Not a valid Nostr identifier: ${original}`);
  }

  switch (decoded.type) {
    case "npub":
      return { pubkey: decoded.data, kind: ROOT_SITE_KIND, identifier: "" };
    case "nprofile":
      return { pubkey: decoded.data.pubkey, kind: ROOT_SITE_KIND, identifier: "" };
    case "naddr":
      return normalize({
        pubkey: decoded.data.pubkey,
        kind: decoded.data.kind,
        identifier: decoded.data.identifier,
      });
    case "nsec":
      throw new Error("That's a private key. Never paste an nsec anywhere.");
    default:
      throw new Error(`A ${decoded.type} does not point to a site. Use an npub or naddr.`);
  }
}

/** Parse a NIP-5A subdomain label: an npub, or base36 pubkey + identifier. */
function parseCanonicalSiteLabel(label: string): NsiteAddress | undefined {
  if (/^npub1/.test(label)) {
    try {
      const decoded = nip19.decode(label);
      if (decoded.type === "npub") {
        return { pubkey: decoded.data, kind: ROOT_SITE_KIND, identifier: "" };
      }
    } catch {
      return undefined;
    }
    return undefined;
  }

  if (!/^[0-9a-z]{50}[a-z0-9-]{1,13}$/.test(label) || label.endsWith("-")) {
    return undefined;
  }

  const pubkey = decodePubkeyB36(label.slice(0, CANONICAL_PUBKEY_LENGTH));
  const identifier = label.slice(CANONICAL_PUBKEY_LENGTH);
  if (!pubkey || !CANONICAL_SITE_IDENTIFIER.test(identifier)) return undefined;

  return { pubkey, kind: NAMED_SITE_KIND, identifier };
}

/** Validate a decomposed address and canonicalize its casing. */
function normalize(address: NsiteAddress): NsiteAddress {
  const pubkey = address.pubkey.toLowerCase();

  if (!HEX64.test(pubkey)) {
    throw new Error("Address contains an invalid pubkey.");
  }
  if (!SITE_KINDS.includes(address.kind)) {
    throw new Error(
      `Kind ${address.kind} is not an nsite. Expected ${ROOT_SITE_KIND} (root site) or ${NAMED_SITE_KIND} (named site).`,
    );
  }
  if (address.kind === NAMED_SITE_KIND && !address.identifier) {
    throw new Error(`Kind ${NAMED_SITE_KIND} requires an identifier.`);
  }
  if (address.kind === ROOT_SITE_KIND && address.identifier) {
    throw new Error(`Kind ${ROOT_SITE_KIND} is a root site and cannot have an identifier.`);
  }

  return { pubkey, kind: address.kind, identifier: address.identifier };
}

/** The canonical `a` tag value, eg `35128:<pubkey>:docs`. */
export function formatNsiteAddress(address: NsiteAddress): string {
  return `${address.kind}:${address.pubkey}:${address.identifier}`;
}

/** An `naddr` for the address, useful for links and copy buttons. */
export function encodeNaddr(address: NsiteAddress, relays?: string[]): string {
  return nip19.naddrEncode({
    pubkey: address.pubkey,
    kind: address.kind,
    identifier: address.identifier,
    relays,
  });
}

/** The NIP-5A subdomain label, or undefined if the identifier is too long. */
export function formatSubdomainLabel(address: NsiteAddress): string | undefined {
  if (!address.identifier) return nip19.npubEncode(address.pubkey);

  if (CANONICAL_SITE_IDENTIFIER.test(address.identifier)) {
    const encoded = encodePubkeyB36(address.pubkey);
    if (encoded) return `${encoded}${address.identifier}`;
  }

  return undefined;
}

export function nsiteAddressEquals(a: NsiteAddress, b: NsiteAddress): boolean {
  return a.pubkey === b.pubkey && a.kind === b.kind && a.identifier === b.identifier;
}

/** Decode a 50-character base36 pubkey label to hex. */
export function decodePubkeyB36(pubkeyB36: string): string | undefined {
  if (!/^[0-9a-z]{50}$/.test(pubkeyB36)) return undefined;

  let value = 0n;
  for (const char of pubkeyB36) {
    const digit = base36Digit(char);
    if (digit === undefined) return undefined;
    value = value * 36n + digit;
    if (value > MAX_PUBKEY) return undefined;
  }

  return value.toString(16).padStart(64, "0");
}

/** Encode a hex pubkey as a 50-character base36 label. */
export function encodePubkeyB36(pubkey: string): string | undefined {
  if (!/^[0-9a-f]{64}$/i.test(pubkey)) return undefined;

  const value = BigInt(`0x${pubkey}`);
  if (value > MAX_PUBKEY) return undefined;
  return value.toString(36).padStart(CANONICAL_PUBKEY_LENGTH, "0");
}

function base36Digit(char: string): bigint | undefined {
  const code = char.charCodeAt(0);
  if (code >= 48 && code <= 57) return BigInt(code - 48);
  if (code >= 97 && code <= 122) return BigInt(code - 87);
  return undefined;
}
