import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { finalizeEvent, generateSecretKey, getPublicKey } from "nostr-tools";
import { type AuthError, MAX_AUTH_AGE, NIP98_KIND, verifyNip98 } from "./auth.ts";

const secretKey = generateSecretKey();
const PUBKEY = getPublicKey(secretKey);
const HOST = "panel.example.com";

interface TokenOpts {
  method?: string;
  url?: string;
  createdAt?: number;
  payload?: string;
  kind?: number;
}

function token(opts: TokenOpts = {}): string {
  const tags: string[][] = [
    ["method", opts.method ?? "GET"],
    ["u", opts.url ?? `https://${HOST}/api/hosts`],
  ];
  if (opts.payload !== undefined) {
    tags.push(["payload", createHash("sha256").update(opts.payload).digest("hex")]);
  }

  const event = finalizeEvent(
    {
      kind: opts.kind ?? NIP98_KIND,
      created_at: opts.createdAt ?? Math.floor(Date.now() / 1000),
      tags,
      content: "",
    },
    secretKey,
  );

  return Buffer.from(JSON.stringify(event)).toString("base64");
}

/** A request as it arrives from a reverse proxy: plain http, internal port. */
function request(opts: { method?: string; path?: string; body?: string; auth?: string } = {}): Request {
  const headers = new Headers();
  if (opts.auth !== undefined) headers.set("authorization", opts.auth);

  return new Request(`http://127.0.0.1:3000${opts.path ?? "/api/hosts"}`, {
    method: opts.method ?? "GET",
    headers,
    body: opts.body,
  });
}

test("accepts a valid token signed for the public URL", async () => {
  const event = await verifyNip98(request({ auth: `Nostr ${token()}` }), { dashboardHost: HOST });
  assert.equal(event.pubkey, PUBKEY);
});

test("requires an Authorization header in the Nostr scheme", async () => {
  await assert.rejects(() => verifyNip98(request(), { dashboardHost: HOST }), /Missing Authorization/);
  await assert.rejects(() => verifyNip98(request({ auth: "Bearer abc" }), { dashboardHost: HOST }), /Nostr scheme/);
  await assert.rejects(
    () => verifyNip98(request({ auth: "Nostr not-base64-json" }), { dashboardHost: HOST }),
    /base64-encoded event/,
  );
});

test("rejects the wrong kind", async () => {
  await assert.rejects(
    () => verifyNip98(request({ auth: `Nostr ${token({ kind: 1 })}` }), { dashboardHost: HOST }),
    /kind 27235/,
  );
});

test("rejects a forged signature", async () => {
  const decoded = JSON.parse(Buffer.from(token(), "base64").toString());
  decoded.pubkey = "f".repeat(64);
  const forged = Buffer.from(JSON.stringify(decoded)).toString("base64");

  await assert.rejects(
    () => verifyNip98(request({ auth: `Nostr ${forged}` }), { dashboardHost: HOST }),
    /signature is invalid/,
  );
});

test("rejects stale and future-dated events", async () => {
  const now = Math.floor(Date.now() / 1000);

  await assert.rejects(
    () =>
      verifyNip98(request({ auth: `Nostr ${token({ createdAt: now - MAX_AUTH_AGE - 5 })}` }), {
        dashboardHost: HOST,
      }),
    /expired/,
  );
  await assert.rejects(
    () =>
      verifyNip98(request({ auth: `Nostr ${token({ createdAt: now + MAX_AUTH_AGE + 5 })}` }), {
        dashboardHost: HOST,
      }),
    /expired/,
  );
});

test("rejects a method or path the token was not signed for", async () => {
  await assert.rejects(
    () =>
      verifyNip98(request({ method: "GET", auth: `Nostr ${token({ method: "DELETE" })}` }), {
        dashboardHost: HOST,
      }),
    /method does not match/,
  );
  await assert.rejects(
    () => verifyNip98(request({ path: "/api/users", auth: `Nostr ${token()}` }), { dashboardHost: HOST }),
    /different URL/,
  );
});

test("query strings are part of the signed URL", async () => {
  const url = `https://${HOST}/api/audit-log?limit=50`;
  const event = await verifyNip98(request({ path: "/api/audit-log?limit=50", auth: `Nostr ${token({ url })}` }), {
    dashboardHost: HOST,
  });
  assert.equal(event.pubkey, PUBKEY);

  await assert.rejects(
    () =>
      verifyNip98(request({ path: "/api/audit-log?limit=999", auth: `Nostr ${token({ url })}` }), {
        dashboardHost: HOST,
      }),
    /different URL/,
  );
});

test("rejects a token signed for another host", async () => {
  await assert.rejects(
    () =>
      verifyNip98(request({ auth: `Nostr ${token({ url: "https://evil.example.com/api/hosts" })}` }), {
        dashboardHost: HOST,
      }),
    /not panel.example.com/,
  );
});

test("verifies the payload hash when present", async () => {
  const body = JSON.stringify({ hostname: "docs.example.com" });
  const auth = `Nostr ${token({ method: "POST", payload: body })}`;

  const event = await verifyNip98(request({ method: "POST", body, auth }), { dashboardHost: HOST });
  assert.equal(event.pubkey, PUBKEY);

  await assert.rejects(
    () =>
      verifyNip98(request({ method: "POST", body: '{"hostname":"evil.example.com"}', auth }), {
        dashboardHost: HOST,
      }),
    /payload hash does not match/,
  );
});

test("auth failures carry a 401 status", async () => {
  await assert.rejects(
    () => verifyNip98(request(), { dashboardHost: HOST }),
    (error: AuthError) => {
      assert.equal(error.status, 401);
      return true;
    },
  );
});
