import { parse } from "tldts";

/** A multi-label LDH name: letters, digits, hyphens, no leading or trailing hyphen. */
const HOSTNAME = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/;

/** The one subdomain npanel offers to fold into the domain beneath it. */
const WWW = "www.";

/** Lowercase and drop the trailing dot, since `example.com.` is `example.com`. */
export function normalizeHostname(input: string): string {
  return input.trim().toLowerCase().replace(/\.$/, "");
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
 */
export function isRootDomain(hostname: string): boolean {
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
 * staff paste into a DNS form is worse than showing them nothing.
 */
export function recordName(hostname: string): string | undefined {
  const parsed = parse(hostname);
  if (parsed.domain === null) return undefined;

  return parsed.subdomain || "@";
}
