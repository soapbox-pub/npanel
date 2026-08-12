import process from "node:process";
import { nip19 } from "nostr-tools";
import { Config } from "../src/config.ts";
import { openDatabase } from "../src/db/index.ts";
import { Logger } from "../src/log.ts";

/**
 * Bootstrap the first admin, or promote someone from the command line.
 *
 * There is no other way to get the first account in: npanel is not
 * permissionless, and the dashboard's user management is admin-only.
 *
 *   bun run make-admin npub1...
 */
const input = process.argv[2];

if (!input) {
  console.error("Usage: bun run make-admin <npub or hex pubkey>");
  process.exit(1);
}

const config = new Config({
  get(key) {
    return process.env[key];
  },
});

const log = new Logger(config.logLevel);
const pubkey = parsePubkey(input);
const db = await openDatabase(config.databasePath, log);

const existing = await db.selectFrom("users").selectAll().where("pubkey", "=", pubkey).executeTakeFirst();
const now = Math.floor(Date.now() / 1000);

if (existing?.role === "admin") {
  console.log(`${nip19.npubEncode(pubkey)} is already an admin.`);
} else if (existing) {
  await db.updateTable("users").set({ role: "admin" }).where("pubkey", "=", pubkey).execute();
  await db
    .insertInto("audit_log")
    .values({
      actor_pubkey: pubkey,
      action: "user.update",
      target: pubkey,
      old_value: JSON.stringify({ role: existing.role }),
      new_value: JSON.stringify({ role: "admin" }),
      created_at: now,
    })
    .execute();
  console.log(`Promoted ${nip19.npubEncode(pubkey)} to admin.`);
} else {
  await db.insertInto("users").values({ pubkey, role: "admin", added_by: null, created_at: now }).execute();
  await db
    .insertInto("audit_log")
    .values({
      actor_pubkey: pubkey,
      action: "user.create",
      target: pubkey,
      old_value: null,
      new_value: JSON.stringify({ role: "admin", via: "make-admin" }),
      created_at: now,
    })
    .execute();
  console.log(`Added ${nip19.npubEncode(pubkey)} as an admin.`);
}

await db.destroy();

function parsePubkey(value: string): string {
  const trimmed = value.trim().replace(/^nostr:/i, "");

  if (/^[0-9a-fA-F]{64}$/.test(trimmed)) return trimmed.toLowerCase();

  try {
    const decoded = nip19.decode(trimmed);
    if (decoded.type === "npub") return decoded.data;
    if (decoded.type === "nprofile") return decoded.data.pubkey;
  } catch {
    // Fall through to the error below.
  }

  console.error(`Not a valid pubkey: ${value}`);
  process.exit(1);
}
