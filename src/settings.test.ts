import assert from "node:assert/strict";
import { test } from "node:test";
import { nip19 } from "nostr-tools";
import { openDatabase } from "./db/index.ts";
import { DEFAULT_SETTINGS, SettingsService, parseSetting } from "./settings.ts";

test("seeds defaults on first load and reads them back", async () => {
  const db = await openDatabase(":memory:");
  const settings = await SettingsService.load(db);

  assert.deepEqual(settings.all(), DEFAULT_SETTINGS);

  const rows = await db.selectFrom("settings").selectAll().execute();
  assert.equal(rows.length, Object.keys(DEFAULT_SETTINGS).length);

  await db.destroy();
});

test("update persists, audits, and notifies listeners", async () => {
  const db = await openDatabase(":memory:");
  const settings = await SettingsService.load(db);

  const seen: string[][] = [];
  settings.onChange((keys) => seen.push(keys));

  const changed = await settings.update({ resync_interval: 900 }, "a".repeat(64));

  assert.deepEqual(changed, ["resync_interval"]);
  assert.equal(settings.get("resync_interval"), 900);
  assert.deepEqual(seen, [["resync_interval"]]);

  const audit = await db.selectFrom("audit_log").selectAll().execute();
  assert.equal(audit.length, 1);
  assert.equal(audit[0].action, "settings.update");
  assert.equal(audit[0].target, "resync_interval");
  assert.equal(audit[0].new_value, "900");

  const reloaded = await SettingsService.load(db);
  assert.equal(reloaded.get("resync_interval"), 900);

  await db.destroy();
});

test("unchanged values are not written or announced", async () => {
  const db = await openDatabase(":memory:");
  const settings = await SettingsService.load(db);

  let notified = 0;
  settings.onChange(() => notified++);

  const changed = await settings.update({ resync_interval: DEFAULT_SETTINGS.resync_interval }, "a".repeat(64));

  assert.deepEqual(changed, []);
  assert.equal(notified, 0);
  assert.equal((await db.selectFrom("audit_log").selectAll().execute()).length, 0);

  await db.destroy();
});

test("invalid values are rejected", () => {
  assert.throws(() => parseSetting("relays", []), /At least one relay/);
  assert.throws(() => parseSetting("relays", ["https://example.com"]), /ws:\/\//);
  assert.throws(() => parseSetting("relays", ["not a url"]), /Invalid relay URL/);
  assert.throws(() => parseSetting("max_file_size", 0), /max_file_size/);
  assert.throws(() => parseSetting("resync_interval", 5), /resync_interval/);
  assert.throws(() => parseSetting("blossom_fallback_servers", ["ftp://x.com"]), /http/);
});

test("the contact is normalized to an npub, whatever names it", () => {
  const pubkey = "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d";
  const npub = "npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6";

  assert.equal(parseSetting("contact_npub", npub), npub);
  assert.equal(parseSetting("contact_npub", `nostr:${npub}`), npub);
  assert.equal(parseSetting("contact_npub", pubkey.toUpperCase()), npub);
  assert.equal(parseSetting("contact_npub", ` ${npub} `), npub);
  assert.equal(parseSetting("contact_npub", ""), "");

  assert.equal(parseSetting("contact_npub", nip19.nprofileEncode({ pubkey, relays: ["wss://relay.test"] })), npub);

  assert.throws(() => parseSetting("contact_npub", "alex@example.com"), /Not an npub/);
  assert.throws(() => parseSetting("contact_npub", nip19.noteEncode(pubkey)), /does not name a person/);
  assert.throws(
    () => parseSetting("contact_npub", "nsec1vl029mgpspedva04g90vltkh6fvh240zqtv9k0t9af8935ke9laqsnlfe5"),
    /private key/,
  );
});

test("urls are normalized", () => {
  assert.deepEqual(parseSetting("relays", ["wss://relay.example.com/"]), ["wss://relay.example.com"]);
  assert.deepEqual(parseSetting("blossom_fallback_servers", ["https://blossom.example.com"]), [
    "https://blossom.example.com/",
  ]);
});

test("corrupt rows fall back to defaults instead of failing to boot", async () => {
  const db = await openDatabase(":memory:");
  await db.insertInto("settings").values({ key: "resync_interval", value: '"nonsense"', updated_at: 0 }).execute();

  const settings = await SettingsService.load(db);
  assert.equal(settings.get("resync_interval"), DEFAULT_SETTINGS.resync_interval);

  await db.destroy();
});
