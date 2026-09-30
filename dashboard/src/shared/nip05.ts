/**
 * NIP-05 names, and the document a domain serves them as.
 *
 * The names belong to npanel rather than to any site: a host serves whatever is
 * in the database here, and only falls back to a `nostr.json` in the deployed
 * manifest when there is nothing. Managing them from the dashboard is the point
 * — a `nostr.json` checked into a site's repository is a file every deploy has
 * to remember to carry, and forgetting it silently unnames everyone in it.
 *
 * The dashboard's copy of the rules `src/identifiers.rs` enforces on the
 * server, so the name a form accepts is the name the server will store.
 * Change both together.
 */

/** The one path this is served at. Fixed by the NIP; nothing else answers here. */
export const NIP05_PATH = "/.well-known/nostr.json";

/**
 * Longest name npanel will store.
 *
 * The NIP sets no limit. This one exists so a name stays something a person
 * types in front of an `@`, and it is far beyond any real one.
 */
const MAX_NAME_LENGTH = 64;

/** The characters a name may use, verbatim from NIP-05's local-part rule. */
const NAME_PATTERN = /^[a-z0-9\-_.]+$/;

/** One name a host answers for. */
export interface Nip05Entry {
  /** The local part, lowercased. `_` is the root name, shown as the domain alone. */
  name: string;
  /** Hex pubkey, lowercased — the NIP forbids `npub` here. */
  pubkey: string;
  /** Where to find this pubkey. May be empty; the NIP only recommends it. */
  relays: string[];
}

/** The body of a `/.well-known/nostr.json` response. */
export interface Nip05Document {
  names: Record<string, string>;
  relays?: Record<string, string[]>;
}

/**
 * Accept a NIP-05 local part; always return it lowercased.
 *
 * The NIP restricts a local part to `a-z0-9-_.`, so anything else is refused
 * rather than escaped: a name outside that set is one no client will look up,
 * and storing it would put a row in the database that can never match a query.
 * Case is folded because a lookup is a string comparison against whatever the
 * client's user typed, and nobody types their own capitals consistently.
 */
export function parseNip05Name(input: string): string {
  const name = input.trim().toLowerCase();

  if (!name) {
    throw new Error("Enter a name for this identifier.");
  }
  if (name.length > MAX_NAME_LENGTH) {
    throw new Error(`A name can be at most ${MAX_NAME_LENGTH} characters.`);
  }
  if (!NAME_PATTERN.test(name)) {
    throw new Error(`A name may only use letters, digits, and - _ . — ${name} has something else.`);
  }

  return name;
}

/**
 * Build the document for a set of entries.
 *
 * Two names may point at one pubkey — an alias, or a person who moved — so the
 * relay lists are unioned rather than letting whichever row sorted last decide
 * where that pubkey is found. `relays` is left out entirely when no entry named
 * one, since the NIP makes it optional and an empty object says nothing.
 */
export function nip05Document(entries: Nip05Entry[]): Nip05Document {
  const names: Record<string, string> = {};
  const relays: Record<string, string[]> = {};

  for (const entry of entries) {
    names[entry.name] = entry.pubkey;

    if (!entry.relays.length) continue;

    const seen = new Set(relays[entry.pubkey] ?? []);
    relays[entry.pubkey] = [...seen, ...entry.relays.filter((relay) => !seen.has(relay))];
  }

  return Object.keys(relays).length ? { names, relays } : { names };
}

/**
 * Read a stored relay list.
 *
 * Everything that writes this column encodes an array of strings, so a row that
 * isn't one is corruption rather than a case to handle — and a name that
 * resolves with no relays is a better answer than a lookup that 500s.
 */
export function parseRelayList(json: string): string[] {
  try {
    const parsed: unknown = JSON.parse(json);
    return Array.isArray(parsed) ? parsed.filter((relay): relay is string => typeof relay === "string") : [];
  } catch {
    return [];
  }
}
