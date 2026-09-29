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
  const groups = ipv6Groups(address);
  if (!groups) return true;

  const [first, second] = groups;
  const embedded = (high: number, low: number) =>
    isPrivateIPv4([high >> 8, high & 0xff, low >> 8, low & 0xff].join("."));

  // Every address that carries an IPv4 one inside it is only as public as that
  // address, since that is where a packet to it may end up.
  if (groups.slice(0, 5).every((group) => group === 0)) {
    // ::ffff:0:0/96, IPv4-mapped.
    if (groups[5] === 0xffff) return embedded(groups[6], groups[7]);
    // ::/96, which is `::`, `::1`, and the deprecated IPv4-compatible block.
    if (groups[5] === 0) return true;
  }
  // 64:ff9b::/96, NAT64's well-known prefix: a NAT64 gateway on the network
  // forwards it to the IPv4 address in its last 32 bits.
  if (first === 0x64 && second === 0xff9b && groups.slice(2, 6).every((group) => group === 0)) {
    return embedded(groups[6], groups[7]);
  }
  // 6to4, 2002::/16, whose next 32 bits are an IPv4 address.
  if (first === 0x2002) return embedded(second, groups[2]);

  if (first === 0x64 && second === 0xff9b) return true; // 64:ff9b:1::/48, local-use NAT64
  if (first === 0x2001 && second === 0) return true; // 2001::/32, Teredo, which hides its IPv4 relay
  if (first === 0x2001 && second === 0xdb8) return true; // 2001:db8::/32, documentation
  if (first === 0x100 && groups.slice(1, 4).every((group) => group === 0)) return true; // 100::/64, discard
  if ((first & 0xfe00) === 0xfc00) return true; // fc00::/7 unique local
  if ((first & 0xffc0) === 0xfe80) return true; // fe80::/10 link local
  if ((first & 0xffc0) === 0xfec0) return true; // fec0::/10, site local, deprecated but still routed
  if ((first & 0xff00) === 0xff00) return true; // ff00::/8 multicast

  return false;
}

/**
 * The eight 16-bit groups of an IPv6 address, with `::` expanded and a
 * trailing dotted IPv4 part folded into the last two. Undefined if it can't be
 * read, which callers treat as private.
 */
function ipv6Groups(address: string): number[] | undefined {
  // Drop any zone index (`fe80::1%eth0`).
  let value = address.toLowerCase().split("%")[0];

  const dotted = value.match(/^(.*:)(\d+\.\d+\.\d+\.\d+)$/);
  if (dotted) {
    if (isIP(dotted[2]) !== 4) return undefined;
    const [a, b, c, d] = dotted[2].split(".").map(Number);
    value = `${dotted[1]}${((a << 8) | b).toString(16)}:${((c << 8) | d).toString(16)}`;
  }

  const halves = value.split("::");
  if (halves.length > 2) return undefined;

  const parse = (part: string) => (part ? part.split(":").map((group) => Number.parseInt(group, 16)) : []);
  const head = parse(halves[0]);
  const tail = halves.length === 2 ? parse(halves[1]) : [];
  const missing = 8 - head.length - tail.length;

  if (halves.length === 2 ? missing < 1 : missing !== 0) return undefined;

  const groups = [...head, ...Array<number>(missing).fill(0), ...tail];
  return groups.every((group) => Number.isInteger(group) && group >= 0 && group <= 0xffff) ? groups : undefined;
}
