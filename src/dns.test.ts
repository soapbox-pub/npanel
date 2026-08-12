import assert from "node:assert/strict";
import { test } from "node:test";
import { openDatabase } from "./db/index.ts";
import { DnsTargets, dnsRecordFor } from "./dns.ts";
import { Logger } from "./log.ts";
import { SettingsService } from "./settings.ts";
import { fakeResolver } from "./test/harness.ts";

async function setup(resolver = fakeResolver()) {
  const db = await openDatabase(":memory:");
  const settings = await SettingsService.load(db);
  const dns = new DnsTargets({
    dashboardHost: "panel.example.com",
    settings,
    log: new Logger("error"),
    resolver,
  });

  return { db, settings, dns };
}

test("classifies a target as the record that points at it", () => {
  assert.deepEqual(dnsRecordFor("203.0.113.10"), { type: "A", value: "203.0.113.10" });
  assert.deepEqual(dnsRecordFor("2001:db8::1"), { type: "AAAA", value: "2001:db8::1" });
  assert.deepEqual(dnsRecordFor("gateway.example.com"), { type: "CNAME", value: "gateway.example.com" });
});

test("detects the target from the dashboard hostname's own records", async () => {
  const { db, dns } = await setup(
    fakeResolver({
      resolve4: async (hostname) => (hostname === "panel.example.com" ? ["203.0.113.10"] : []),
      resolve6: async () => ["2001:db8::1"],
    }),
  );

  assert.deepEqual(await dns.target(), {
    source: "dashboard_host",
    records: [
      { type: "A", value: "203.0.113.10" },
      { type: "AAAA", value: "2001:db8::1" },
    ],
  });

  await db.destroy();
});

test("survives a hostname with no records of one family, or none at all", async () => {
  const ipv4Only = await setup(
    fakeResolver({
      resolve4: async () => ["203.0.113.10"],
      resolve6: async () => {
        throw new Error("queryAaaa ENODATA");
      },
    }),
  );

  assert.deepEqual((await ipv4Only.dns.target()).records, [{ type: "A", value: "203.0.113.10" }]);
  await ipv4Only.db.destroy();

  const unresolvable = await setup(
    fakeResolver({
      resolve4: async () => {
        throw new Error("queryA ENOTFOUND");
      },
      resolve6: async () => {
        throw new Error("queryAaaa ENOTFOUND");
      },
    }),
  );

  assert.deepEqual((await unresolvable.dns.target()).records, []);
  await unresolvable.db.destroy();
});

test("only resolves once until the cache expires", async () => {
  let lookups = 0;
  const { db, dns } = await setup(
    fakeResolver({
      resolve4: async () => {
        lookups++;
        return ["203.0.113.10"];
      },
    }),
  );

  await dns.target();
  await dns.target();

  assert.equal(lookups, 1);
  await db.destroy();
});

test("prefers a configured target, and never resolves when one is set", async () => {
  let lookups = 0;
  const { db, settings, dns } = await setup(
    fakeResolver({
      resolve4: async () => {
        lookups++;
        return ["203.0.113.10"];
      },
    }),
  );

  await settings.update({ dns_targets: ["Gateway.Example.com."] }, "test");

  assert.deepEqual(await dns.target(), {
    source: "settings",
    records: [{ type: "CNAME", value: "gateway.example.com" }],
  });
  assert.equal(lookups, 0);

  await db.destroy();
});

test("refuses a CNAME target alongside anything else", async () => {
  const { db, settings } = await setup();

  await assert.rejects(
    () => settings.update({ dns_targets: ["gateway.example.com", "203.0.113.10"] }, "test"),
    /must be the only record/,
  );
  await assert.rejects(() => settings.update({ dns_targets: ["not a hostname"] }, "test"), /Not an IP address/);

  await settings.update({ dns_targets: ["203.0.113.10", "2001:db8::1"] }, "test");
  assert.deepEqual(settings.get("dns_targets"), ["203.0.113.10", "2001:db8::1"]);

  await db.destroy();
});
