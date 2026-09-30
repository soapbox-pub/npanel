import assert from "node:assert/strict";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { after, before, test } from "node:test";
import { Logger } from "../log.ts";
import { Geo } from "./geo.ts";
import { MmdbFile, addressBytes } from "./mmdb.ts";

/** A value the writer below encodes as a pointer to an earlier offset in the data section. */
class Pointer {
  constructor(readonly offset: number) {}
}

type Value = string | number | boolean | Pointer | Value[] | { [key: string]: Value };

function control(type: number, size: number): number[] {
  assert.ok(size < 29, "the test writer only encodes short fields");
  return type > 7 ? [size, type - 7] : [(type << 5) | size];
}

function encode(value: Value): Buffer {
  if (value instanceof Pointer) {
    assert.ok(value.offset < 2048);
    return Buffer.from([(1 << 5) | (value.offset >> 8), value.offset & 0xff]);
  }
  if (typeof value === "string") {
    const bytes = Buffer.from(value);
    return Buffer.concat([Buffer.from(control(2, bytes.length)), bytes]);
  }
  if (typeof value === "boolean") return Buffer.from(control(14, value ? 1 : 0));
  if (typeof value === "number") {
    if (!Number.isInteger(value)) {
      const bytes = Buffer.alloc(8);
      bytes.writeDoubleBE(value);
      return Buffer.concat([Buffer.from(control(3, 8)), bytes]);
    }
    const bytes = Buffer.alloc(4);
    bytes.writeUInt32BE(value);
    return Buffer.concat([Buffer.from(control(6, 4)), bytes]);
  }
  if (Array.isArray(value)) {
    return Buffer.concat([Buffer.from(control(11, value.length)), ...value.map(encode)]);
  }
  const entries = Object.entries(value);
  return Buffer.concat([
    Buffer.from(control(7, entries.length)),
    ...entries.flatMap(([k, v]) => [encode(k), encode(v)]),
  ]);
}

type Side = { node: number } | { data: number } | undefined;

/** A MaxMind DB file mapping each network to the data-section offset of its record. */
function buildDatabase(
  ipVersion: 4 | 6,
  recordSize: 24 | 28 | 32,
  data: Buffer,
  networks: { address: string; prefix: number; offset: number }[],
): Buffer {
  const nodes: [Side, Side][] = [[undefined, undefined]];

  for (const { address, prefix, offset } of networks) {
    const bytes = addressBytes(address) as Uint8Array;
    // IPv4 networks in an IPv6 tree sit under 96 zero bits.
    const lead = ipVersion === 6 && bytes.length === 4 ? 96 : 0;
    const bits = lead + prefix;

    let node = 0;
    for (let i = 0; i < bits; i++) {
      const bit = i < lead ? 0 : (bytes[(i - lead) >> 3] >> (7 - ((i - lead) & 7))) & 1;
      if (i === bits - 1) {
        nodes[node][bit] = { data: offset };
        break;
      }
      const next = nodes[node][bit];
      if (next && "node" in next) {
        node = next.node;
      } else {
        nodes.push([undefined, undefined]);
        nodes[node][bit] = { node: nodes.length - 1 };
        node = nodes.length - 1;
      }
    }
  }

  const count = nodes.length;
  const value = (side: Side) => (!side ? count : "node" in side ? side.node : count + 16 + side.data);

  const nodeBytes = recordSize / 4;
  const tree = Buffer.alloc(count * nodeBytes);
  nodes.forEach(([l, r], i) => {
    const left = value(l);
    const right = value(r);
    const at = i * nodeBytes;
    if (recordSize === 24) {
      tree.writeUIntBE(left, at, 3);
      tree.writeUIntBE(right, at + 3, 3);
    } else if (recordSize === 28) {
      tree.writeUIntBE(left & 0xffffff, at, 3);
      tree[at + 3] = ((left >> 24) << 4) | (right >> 24);
      tree.writeUIntBE(right & 0xffffff, at + 4, 3);
    } else {
      tree.writeUInt32BE(left, at);
      tree.writeUInt32BE(right, at + 4);
    }
  });

  const metadata = encode({
    node_count: count,
    record_size: recordSize,
    ip_version: ipVersion,
    database_type: "Test-City",
    build_epoch: 1_788_000_000,
    binary_format_major_version: 2,
  });

  return Buffer.concat([tree, Buffer.alloc(16), data, Buffer.from("abcdef4d61784d696e642e636f6d", "hex"), metadata]);
}

/** Two city records sharing their country through a pointer, the way real databases do. */
function cityData(): { data: Buffer; mountainView: number; toronto: number; tor: number } {
  const us = encode({ iso_code: "US", names: { en: "United States" } });
  const usAt = 0;
  const mountainView = us.length;
  const mv = encode({
    city: { names: { en: "Mountain View" } },
    country: new Pointer(usAt),
    location: { latitude: 37.4, longitude: -122.08 },
    subdivisions: [{ names: { en: "California" } }],
    is_anycast: true,
  });
  const toronto = mountainView + mv.length;
  const to = encode({
    city: { names: { en: "Toronto" } },
    country: { iso_code: "CA" },
    subdivisions: [{ names: { en: "Ontario" } }],
  });
  const tor = toronto + to.length;
  const t1 = encode({ country: { iso_code: "T1" } });
  return { data: Buffer.concat([us, mv, to, t1]), mountainView, toronto, tor };
}

let dir: string;

before(async () => {
  dir = await mkdtemp(join(tmpdir(), "npanel-mmdb-"));
});

after(async () => {
  await rm(dir, { recursive: true, force: true });
});

async function write(name: string, bytes: Buffer): Promise<string> {
  const path = join(dir, name);
  await writeFile(path, bytes);
  return path;
}

for (const recordSize of [24, 28, 32] as const) {
  test(`${recordSize}-bit records: IPv4 and IPv6 addresses find their networks in an IPv6 tree`, async () => {
    const { data, mountainView, toronto } = cityData();
    const path = await write(
      `v6-${recordSize}.mmdb`,
      buildDatabase(6, recordSize, data, [
        { address: "8.8.8.0", prefix: 24, offset: mountainView },
        { address: "2607:f8b0::", prefix: 32, offset: toronto },
      ]),
    );

    const db = new MmdbFile(path);
    try {
      assert.equal(db.metadata.recordSize, recordSize);
      assert.equal(db.metadata.ipVersion, 6);
      assert.equal(db.metadata.databaseType, "Test-City");
      assert.equal(db.metadata.buildEpoch.getTime(), 1_788_000_000_000);

      assert.deepEqual(db.get("8.8.8.8"), {
        city: { names: { en: "Mountain View" } },
        country: { iso_code: "US", names: { en: "United States" } },
        location: { latitude: 37.4, longitude: -122.08 },
        subdivisions: [{ names: { en: "California" } }],
        is_anycast: true,
      });
      assert.deepEqual((db.get("2607:f8b0:4004::1") as { city: unknown }).city, { names: { en: "Toronto" } });

      assert.equal(db.get("8.8.9.1"), null);
      assert.equal(db.get("2001:db8::1"), null);
      assert.equal(db.get("not an address"), null);
    } finally {
      db.close();
    }
  });
}

test("an IPv4-only tree finds IPv4 addresses and has no answer for IPv6", async () => {
  const { data, toronto } = cityData();
  const path = await write(
    "v4.mmdb",
    buildDatabase(4, 24, data, [{ address: "192.0.2.0", prefix: 24, offset: toronto }]),
  );

  const db = new MmdbFile(path);
  try {
    assert.equal((db.get("192.0.2.77") as { country: { iso_code: string } }).country.iso_code, "CA");
    assert.equal(db.get("::ffff:192.0.2.77"), null);
  } finally {
    db.close();
  }
});

test("a file without MaxMind metadata is refused", async () => {
  const path = await write("junk.mmdb", Buffer.from("not a database at all"));
  assert.throws(() => new MmdbFile(path), /no metadata marker/);
});

test("addresses become the bytes the tree is walked by", () => {
  assert.deepEqual(addressBytes("192.0.2.1"), Uint8Array.from([192, 0, 2, 1]));
  assert.deepEqual(addressBytes("::"), new Uint8Array(16));
  assert.deepEqual(addressBytes("2001:db8::1"), Uint8Array.from([0x20, 1, 0x0d, 0xb8, ...Array(11).fill(0), 1]));
  assert.deepEqual(addressBytes("::ffff:192.0.2.1"), Uint8Array.from([...Array(10).fill(0), 0xff, 0xff, 192, 0, 2, 1]));
  assert.deepEqual(addressBytes("fe80::1%eth0"), Uint8Array.from([0xfe, 0x80, ...Array(13).fill(0), 1]));
  assert.equal(addressBytes("example.com"), null);
});

test("Geo reads a location out of the City database, and none out of Tor", async () => {
  const geoDir = join(dir, "geo");
  const { data, mountainView, tor } = cityData();
  await mkdir(geoDir);
  await writeFile(
    join(geoDir, "dbip-city-lite.mmdb"),
    buildDatabase(6, 28, data, [
      { address: "8.8.8.0", prefix: 24, offset: mountainView },
      { address: "185.220.101.0", prefix: 24, offset: tor },
    ]),
  );

  const geo = new Geo(geoDir, new Logger("error"));
  await geo.load();
  try {
    assert.deepEqual(geo.lookup("8.8.8.8"), { country: "US", region: "California", city: "Mountain View" });
    assert.deepEqual(geo.lookup("185.220.101.1"), { country: "", region: "", city: "" });
    assert.deepEqual(geo.lookup("203.0.113.1"), { country: "", region: "", city: "" });
  } finally {
    geo.stop();
  }
});
