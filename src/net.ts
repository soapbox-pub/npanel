import { lookup } from "node:dns/promises";
import { isIP } from "node:net";

/**
 * Whether a URL points somewhere on the public internet.
 *
 * Blob download targets come from places npanel does not control — a
 * manifest's `server` tags, the author's kind 10063 list, and any redirect
 * those servers hand back — so an nsite author can otherwise aim npanel at
 * anything reachable from the box it runs on. The response body can't leak
 * (it has to hash to the blob the manifest asked for, or it's discarded), but
 * the status code and timing of the attempt land in the deploy log, which
 * turns the deploy queue into an internal-network scanner.
 *
 * The hostname is resolved and *every* address it answers with must be public;
 * a name resolving to both a public and a private address is refused. This
 * still races DNS: the address checked here is not necessarily the one the
 * connection uses. Closing that gap needs a custom socket factory per runtime,
 * which npanel doesn't have — so treat this as raising the cost, not as an
 * airtight boundary, and don't run npanel somewhere that a GET request to a
 * neighbouring service would be catastrophic.
 */
export async function isPublicUrl(url: URL): Promise<boolean> {
  for (const address of await resolveAll(url.hostname)) {
    if (!isPublicAddress(address)) return false;
  }

  return true;
}

/** Every IP a URL hostname resolves to. Throws when it resolves to nothing. */
async function resolveAll(hostname: string): Promise<string[]> {
  // A URL keeps IPv6 literals in brackets; `isIP` and DNS both want them bare.
  const host = hostname.replace(/^\[/, "").replace(/\]$/, "");

  if (isIP(host)) return [host];

  const answers = await lookup(host, { all: true });
  if (!answers.length) throw new Error(`${hostname} does not resolve`);

  return answers.map((answer) => answer.address);
}

/**
 * Whether an IP address is routable on the public internet.
 *
 * Anything unparseable is treated as private: the point is to fail closed.
 */
export function isPublicAddress(address: string): boolean {
  switch (isIP(address)) {
    case 4:
      return !isPrivateIPv4(address);
    case 6:
      return !isPrivateIPv6(address);
    default:
      return false;
  }
}

function isPrivateIPv4(address: string): boolean {
  const [a, b] = address.split(".").map(Number);

  if (a === 0) return true; // 0.0.0.0/8, "this network"
  if (a === 10) return true; // private
  if (a === 127) return true; // loopback
  if (a === 100 && b >= 64 && b <= 127) return true; // carrier-grade NAT
  if (a === 169 && b === 254) return true; // link-local, incl. cloud metadata
  if (a === 172 && b >= 16 && b <= 31) return true; // private
  if (a === 192 && b === 0) return true; // protocol assignments
  if (a === 192 && b === 168) return true; // private
  if (a === 198 && (b === 18 || b === 19)) return true; // benchmarking
  if (a >= 224) return true; // multicast and reserved, incl. broadcast

  return false;
}

function isPrivateIPv6(address: string): boolean {
  // Drop any zone index (`fe80::1%eth0`).
  const value = address.toLowerCase().split("%")[0];

  if (value === "::" || value === "::1") return true;

  // IPv4-mapped addresses are just IPv4 with a prefix, in either notation.
  if (value.startsWith("::ffff:")) {
    const mapped = value.slice("::ffff:".length);
    if (isIP(mapped) === 4) return isPrivateIPv4(mapped);

    const groups = mapped.split(":");
    if (groups.length === 2) {
      const high = Number.parseInt(groups[0], 16);
      const low = Number.parseInt(groups[1], 16);
      if (Number.isNaN(high) || Number.isNaN(low)) return true;
      return isPrivateIPv4([high >> 8, high & 0xff, low >> 8, low & 0xff].join("."));
    }

    return true;
  }

  const first = Number.parseInt(value.split(":")[0] || "0", 16);
  if (Number.isNaN(first)) return true;

  if ((first & 0xfe00) === 0xfc00) return true; // fc00::/7 unique local
  if ((first & 0xffc0) === 0xfe80) return true; // fe80::/10 link local
  if ((first & 0xff00) === 0xff00) return true; // ff00::/8 multicast

  return false;
}
