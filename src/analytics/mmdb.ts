import { closeSync, fstatSync, openSync, readSync } from "node:fs";
import { isIP } from "node:net";

/**
 * A MaxMind DB file, read a page at a time instead of whole.
 *
 * A lookup touches one node per bit of the address and then one record, so of
 * a hundred-odd megabytes it reads a few kilobytes. Holding the file in memory
 * would make every one of those megabytes resident for the life of the thread;
 * reading pages through the kernel leaves them in the page cache, where they
 * stay hot while they're used and can be reclaimed when they aren't. The pages
 * kept here are only the tree's upper levels, which every lookup walks.
 *
 * Only as much of the format as a lookup needs, per
 * https://maxmind.github.io/MaxMind-DB/.
 */
export class MmdbFile {
  readonly metadata: MmdbMetadata;
  private readonly pages: PagedFile;
  private readonly nodeBytes: number;
  private readonly dataStart: number;
  private readonly ipv4Root: number;

  constructor(path: string) {
    this.pages = new PagedFile(path);
    try {
      this.metadata = readMetadata(this.pages);
    } catch (error) {
      this.pages.close();
      throw error;
    }

    this.nodeBytes = this.metadata.recordSize / 4;
    this.dataStart = (this.metadata.nodeCount * this.metadata.recordSize) / 4 + DATA_SEPARATOR;
    this.ipv4Root = this.findIpv4Root();
  }

  /** The record for the network `ip` is in, or null if the file has none. */
  get(ip: string): unknown {
    const address = addressBytes(ip);
    if (!address) return null;
    // An IPv6 address has nowhere to go in an IPv4-only tree.
    if (address.length === 16 && this.metadata.ipVersion === 4) return null;

    const { nodeCount } = this.metadata;
    let node = address.length === 4 ? this.ipv4Root : 0;

    for (let bit = 0; bit < address.length * 8 && node < nodeCount; bit++) {
      const right = (address[bit >> 3] >> (7 - (bit & 7))) & 1;
      node = this.record(node, right === 1);
    }

    // Exactly `nodeCount` is the tree's way of saying "not found".
    if (node <= nodeCount) return null;
    return new Decoder(this.pages, this.dataStart).value(this.dataStart + node - nodeCount - DATA_SEPARATOR);
  }

  close(): void {
    this.pages.close();
  }

  private record(node: number, right: boolean): number {
    const b = this.pages.bytes(node * this.nodeBytes, this.nodeBytes);

    switch (this.metadata.recordSize) {
      case 24:
        return right ? b.readUIntBE(3, 3) : b.readUIntBE(0, 3);
      case 28:
        return right ? ((b[3] & 0x0f) << 24) | b.readUIntBE(4, 3) : ((b[3] & 0xf0) << 20) | b.readUIntBE(0, 3);
      default:
        return right ? b.readUInt32BE(4) : b.readUInt32BE(0);
    }
  }

  /** IPv4 addresses live in an IPv6 tree under 96 zero bits, walked once rather than per lookup. */
  private findIpv4Root(): number {
    if (this.metadata.ipVersion === 4) return 0;

    let node = 0;
    for (let i = 0; i < 96 && node < this.metadata.nodeCount; i++) {
      node = this.record(node, false);
    }
    return node;
  }
}

export interface MmdbMetadata {
  nodeCount: number;
  recordSize: 24 | 28 | 32;
  ipVersion: 4 | 6;
  databaseType: string;
  buildEpoch: Date;
}

/** Sixteen zero bytes between the search tree and the data section. */
const DATA_SEPARATOR = 16;

/** `\xAB\xCD\xEF` then `MaxMind.com`, which begins the metadata at the end of the file. */
const METADATA_MARKER = Buffer.from("abcdef4d61784d696e642e636f6d", "hex");

/** The metadata is within this many bytes of the end, by the format's own rule. */
const METADATA_MAX = 128 * 1024;

function readMetadata(pages: PagedFile): MmdbMetadata {
  const tailStart = Math.max(0, pages.size - METADATA_MAX);
  const tail = pages.bytes(tailStart, pages.size - tailStart);
  const at = tail.lastIndexOf(METADATA_MARKER);
  if (at === -1) throw new Error("Not a MaxMind DB file: no metadata marker.");

  const start = tailStart + at + METADATA_MARKER.length;
  const raw = new Decoder(pages, start).value(start);
  if (!isMap(raw)) throw new Error("Not a MaxMind DB file: metadata is not a map.");

  const { node_count, record_size, ip_version, database_type, build_epoch } = raw;
  if (typeof node_count !== "number" || (record_size !== 24 && record_size !== 28 && record_size !== 32)) {
    throw new Error(`Unsupported MaxMind DB record size: ${String(record_size)}.`);
  }
  if (ip_version !== 4 && ip_version !== 6) {
    throw new Error(`Unsupported MaxMind DB IP version: ${String(ip_version)}.`);
  }

  return {
    nodeCount: node_count,
    recordSize: record_size,
    ipVersion: ip_version,
    databaseType: typeof database_type === "string" ? database_type : "",
    buildEpoch: new Date(Number(build_epoch) * 1000),
  };
}

function isMap(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

enum Type {
  Extended = 0,
  Pointer = 1,
  Utf8String = 2,
  Double = 3,
  Bytes = 4,
  Uint16 = 5,
  Uint32 = 6,
  Map = 7,
  Int32 = 8,
  Uint64 = 9,
  Uint128 = 10,
  Array = 11,
  Container = 12,
  EndMarker = 13,
  Boolean = 14,
  Float = 15,
}

/** Decodes the data section, where every pointer counts from `base`. */
class Decoder {
  private offset = 0;

  constructor(
    private readonly pages: PagedFile,
    private readonly base: number,
  ) {}

  value(offset: number): unknown {
    this.offset = offset;
    return this.next();
  }

  private next(): unknown {
    const ctrl = this.byte();
    let type = ctrl >> 5;

    if (type === Type.Pointer) {
      const target = this.pointer(ctrl);
      const resume = this.offset;
      // A pointer never points at another pointer, so this recursion is one deep.
      const value = this.value(this.base + target);
      this.offset = resume;
      return value;
    }

    if (type === Type.Extended) type = 7 + this.byte();
    const size = this.size(ctrl & 0x1f);

    switch (type) {
      case Type.Utf8String:
        return this.take(size).toString("utf8");
      case Type.Double:
        return this.take(8).readDoubleBE(0);
      case Type.Float:
        return this.take(4).readFloatBE(0);
      case Type.Bytes:
        return Uint8Array.from(this.take(size));
      case Type.Uint16:
      case Type.Uint32:
        return size === 0 ? 0 : this.take(size).readUIntBE(0, size);
      case Type.Int32:
        return size === 0 ? 0 : this.take(size).readIntBE(0, size);
      case Type.Uint64:
      case Type.Uint128: {
        let n = 0n;
        for (const byte of this.take(size)) n = (n << 8n) | BigInt(byte);
        return n;
      }
      case Type.Boolean:
        return size !== 0;
      case Type.Map: {
        const map: Record<string, unknown> = {};
        for (let i = 0; i < size; i++) {
          const key = this.next();
          map[String(key)] = this.next();
        }
        return map;
      }
      case Type.Array: {
        const array: unknown[] = [];
        for (let i = 0; i < size; i++) array.push(this.next());
        return array;
      }
    }

    throw new Error(`Unknown MaxMind DB data type ${type} at offset ${this.offset}.`);
  }

  private pointer(ctrl: number): number {
    const size = (ctrl >> 3) & 0x3;
    const high = ctrl & 0x7;

    switch (size) {
      case 0:
        return (high << 8) | this.byte();
      case 1:
        return ((high << 16) | this.take(2).readUInt16BE(0)) + 2048;
      case 2:
        return ((high << 24) | this.take(3).readUIntBE(0, 3)) + 526336;
      default:
        return this.take(4).readUInt32BE(0);
    }
  }

  private size(low: number): number {
    if (low < 29) return low;
    if (low === 29) return 29 + this.byte();
    if (low === 30) return 285 + this.take(2).readUInt16BE(0);
    return 65821 + this.take(3).readUIntBE(0, 3);
  }

  private byte(): number {
    return this.take(1)[0];
  }

  private take(length: number): Buffer {
    const bytes = this.pages.bytes(this.offset, length);
    this.offset += length;
    return bytes;
  }
}

const PAGE = 4096;

/**
 * Pages kept between lookups: 4 MB. Enough for the tree's upper levels, which
 * every lookup shares, and for the handful of records popular networks map to.
 */
const MAX_PAGES = 1024;

/** Positioned reads over a file, a page at a time, keeping the recently used pages. */
class PagedFile {
  readonly size: number;
  private readonly fd: number;
  private readonly cache = new Map<number, Buffer>();

  constructor(path: string) {
    this.fd = openSync(path, "r");
    this.size = fstatSync(this.fd).size;
  }

  bytes(offset: number, length: number): Buffer {
    if (offset < 0 || offset + length > this.size) {
      throw new Error(`Read past the end of the MaxMind DB file at offset ${offset}.`);
    }

    const first = Math.floor(offset / PAGE);
    const last = Math.floor((offset + length - 1) / PAGE);
    if (first === last) {
      const start = offset - first * PAGE;
      return this.page(first).subarray(start, start + length);
    }

    const out = Buffer.allocUnsafe(length);
    let written = 0;
    for (let n = first; n <= last; n++) {
      const page = this.page(n);
      const start = n === first ? offset - n * PAGE : 0;
      const end = Math.min(page.length, offset + length - n * PAGE);
      written += page.copy(out, written, start, end);
    }
    return out;
  }

  close(): void {
    closeSync(this.fd);
    this.cache.clear();
  }

  private page(n: number): Buffer {
    const cached = this.cache.get(n);
    if (cached) {
      // Re-inserted, so eviction below drops the least recently used.
      this.cache.delete(n);
      this.cache.set(n, cached);
      return cached;
    }

    const page = Buffer.allocUnsafe(Math.min(PAGE, this.size - n * PAGE));
    readSync(this.fd, page, 0, page.length, n * PAGE);

    if (this.cache.size >= MAX_PAGES) {
      const oldest = this.cache.keys().next().value;
      if (oldest !== undefined) this.cache.delete(oldest);
    }
    this.cache.set(n, page);
    return page;
  }
}

/** An address as the bytes the tree is walked by: 4 for IPv4, 16 for IPv6. */
export function addressBytes(ip: string): Uint8Array | null {
  const bare = ip.replace(/%.*$/, "");
  const version = isIP(bare);
  if (version === 4) return Uint8Array.from(bare.split(".").map(Number));
  if (version !== 6) return null;

  // An embedded IPv4 tail, as in `::ffff:192.0.2.1`, is two groups written differently.
  let text = bare;
  const dotted = text.match(/(\d+\.\d+\.\d+\.\d+)$/);
  if (dotted) {
    const [a, b, c, d] = dotted[1].split(".").map(Number);
    text = `${text.slice(0, -dotted[1].length)}${((a << 8) | b).toString(16)}:${((c << 8) | d).toString(16)}`;
  }

  const [head, tail] = text.split("::");
  const left = head ? head.split(":") : [];
  const right = tail ? tail.split(":") : [];
  const groups = tail === undefined ? left : [...left, ...Array(8 - left.length - right.length).fill("0"), ...right];

  const bytes = new Uint8Array(16);
  groups.forEach((group, i) => {
    const n = Number.parseInt(group, 16);
    bytes[i * 2] = n >> 8;
    bytes[i * 2 + 1] = n & 0xff;
  });
  return bytes;
}
