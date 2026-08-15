/** A multi-label LDH name: letters, digits, hyphens, no leading or trailing hyphen. */
const HOSTNAME = /^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/;

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
