// The dashboard's twin of the server's `src/hostname.rs`: the same rules, so what
// the dashboard shows is what the server will do. Change both together.

import { parse } from "tldts";

/** A multi-label LDH name: letters, digits, hyphens, no leading or trailing hyphen. */
const HOSTNAME = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/;

/** The one subdomain npanel offers to fold into the domain beneath it. */
const WWW = "www.";

/**
 * A v3 onion address: 56 characters of base32, then `.onion`.
 *
 * The label is a public key, a version byte and a checksum, so the length and
 * alphabet are fixed and there is no such thing as a short one. v2 addresses
 * were 16 characters; Tor stopped supporting them in 2021 and they are
 * deliberately not matched here.
 */
const ONION = /^[a-z2-7]{56}\.onion$/;

/** Whether an already-normalized string is a v3 onion address. */
export function isOnionAddress(value: string): boolean {
  return ONION.test(value);
}

/** Lowercase and drop the trailing dot, since `example.com.` is `example.com`. */
export function normalizeHostname(input: string): string {
  return input.trim().toLowerCase().replace(/\.$/, "");
}

/**
 * The hostname a request asked for, from its `Host` header.
 *
 * `X-Forwarded-Host` is deliberately not consulted. npanel has no way to tell
 * a proxy's copy of that header from a client's, and not every proxy sets one
 * — nginx does not — so honouring it would let a visitor to any site reach the
 * dashboard and the whole JSON API by adding one header. A proxy that rewrites
 * `Host` has to be configured not to; see docs/deployment.md.
 *
 * That is also what lets both listeners share this function. Tor hands a
 * visitor's bytes on unaltered, so a listener that read forwarding headers
 * would be reading the visitor's own.
 *
 * The port a client may have appended is dropped, along with the trailing dot
 * of a fully-qualified name.
 */
export function requestHostname(request: Request): string {
  // Port first: in `example.com.:443` the trailing dot is not trailing yet.
  return normalizeHostname((request.headers.get("host") ?? "").trim().replace(/:\d+$/, ""));
}

/** Whether an already-normalized string is a usable hostname. */
export function isHostname(value: string): boolean {
  return value.length <= 253 && HOSTNAME.test(value);
}

/** Whether a hostname is, or sits under, a suffix. */
export function matchesSuffix(hostname: string, suffix: string): boolean {
  return hostname === suffix || hostname.endsWith(`.${suffix}`);
}

/**
 * Parse a hostname staff typed into the dashboard.
 *
 * Hostnames must be resolvable names, not URLs or wildcards. `allowedSuffixes`
 * comes from the setting of the same name and is required rather than
 * optional, because a caller that forgot it would silently let anyone
 * authorize a certificate for any domain — see the `allowed_host_suffixes`
 * section of docs/configuration.md.
 */
export function parseHostname(input: string, allowedSuffixes: string[]): string {
  const hostname = normalizeHostname(input);

  if (!hostname) {
    throw new Error("Enter a domain.");
  }
  if (!HOSTNAME.test(hostname)) {
    throw new Error(`Not a valid domain: ${input.trim()}`);
  }
  if (hostname.length > 253) {
    throw new Error("Domain is too long.");
  }
  if (hostname.endsWith(".onion")) {
    // npanel mints these itself, one per site, and a key it did not generate is
    // an address it cannot answer at. Matched on the suffix rather than on
    // `isOnionAddress` so that a mistyped address gets this sentence too,
    // instead of being read as an ordinary domain under an unusual TLD.
    throw new Error("npanel gives each site its own .onion address. Add the site's regular domain instead.");
  }
  if (allowedSuffixes.length && !allowedSuffixes.some((suffix) => matchesSuffix(hostname, suffix))) {
    throw new Error(
      `${hostname} is outside the domains this gateway serves (${allowedSuffixes.join(", ")}). An admin can change that under Settings.`,
    );
  }

  return hostname;
}

/**
 * Whether a hostname is a registrable domain with nothing in front of it —
 * `example.com` or `example.co.uk`, but not `docs.example.com`.
 *
 * Label counting cannot answer this: `example.co.uk` has three labels and
 * `foo.github.io` has three too, and they are not the same shape. So it goes
 * through the Public Suffix List (`tldts`), which is the only thing that knows
 * where the registry ends and the registrant begins. A name whose suffix isn't
 * in the list at all — `localhost`, an IP address — is not a root domain.
 *
 * This is what gates the www redirect: folding `www.docs.example.com` into
 * `docs.example.com` is a guess about someone else's subdomain layout, while
 * doing it for an apex is the near-universal convention.
 *
 * Onion addresses are excluded even though `onion` is in the list. Tor reads
 * the whole label as a key, so `www.<address>.onion` is not a name with a
 * subdomain in front of it — it is not an onion address at all, and no Tor
 * client will resolve it. Without this the dashboard would offer a www switch
 * for every site's onion and the redirect would point into nothing.
 */
export function isRootDomain(hostname: string): boolean {
  if (hostname.endsWith(".onion")) return false;

  const parsed = parse(hostname);
  return parsed.domain !== null && !parsed.subdomain;
}

/**
 * The root domain a `www.` hostname belongs to, if it is one.
 *
 * `undefined` for anything else, including `www.docs.example.com`, whose apex
 * is not what the label in front of it suggests.
 */
export function wwwApex(hostname: string): string | undefined {
  if (!hostname.startsWith(WWW)) return undefined;

  const apex = hostname.slice(WWW.length);
  return isRootDomain(apex) ? apex : undefined;
}

/**
 * What to type in the "name" field at a DNS provider for this hostname:
 * `@` for a root domain, the subdomain otherwise.
 *
 * `undefined` when the name has no recognizable public suffix, since a guess
 * staff paste into a DNS form is worse than showing them nothing — and for an
 * onion address, which is reached without DNS at all.
 */
export function recordName(hostname: string): string | undefined {
  if (hostname.endsWith(".onion")) return undefined;

  const parsed = parse(hostname);
  if (parsed.domain === null) return undefined;

  return parsed.subdomain || "@";
}
