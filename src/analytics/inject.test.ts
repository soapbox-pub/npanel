import assert from "node:assert/strict";
import { test } from "node:test";
import { allowAnalytics, injectAnalytics } from "./inject.ts";

const HASH = "sha384-abc";
const TAG = '<script type="module" src="/.npanel/script.js"></script>';

function inject(html: string): string | undefined {
  const out = injectAnalytics(Buffer.from(html, "latin1"), HASH);
  return out && Buffer.from(out).toString("latin1");
}

test("the tracker goes at the end of the head", () => {
  assert.equal(
    inject("<!doctype html><html><head><title>x</title></head><body>hi</body></html>"),
    `<!doctype html><html><head><title>x</title>${TAG}</head><body>hi</body></html>`,
  );
  // Whatever case the site wrote its tags in.
  assert.equal(inject("<HTML><HEAD></HEAD></HTML>"), `<HTML><HEAD>${TAG}</HEAD></HTML>`);
});

test("a document without a head gets it where the head would have ended", () => {
  assert.equal(inject("<html><body class=a>hi</body></html>"), `<html>${TAG}<body class=a>hi</body></html>`);
  assert.equal(inject('<html lang="en"><p>hi'), `<html lang="en">${TAG}<p>hi`);
  assert.equal(inject("<!DOCTYPE html><title>x</title>"), `<!DOCTYPE html>${TAG}<title>x</title>`);
});

test("a fragment is left alone", () => {
  assert.equal(inject("<p>A partial some page fetches and inserts.</p>"), undefined);
  // `<bodyguard>` is not a body.
  assert.equal(inject("<bodyguard>hi</bodyguard>"), undefined);
});

test("bytes outside ASCII come back exactly as they were", () => {
  // Latin-1 é, then UTF-8 é: neither is re-encoded.
  const page = Buffer.concat([
    Buffer.from("<html><head><title>"),
    Buffer.from([0xe9, 0xc3, 0xa9]),
    Buffer.from("</title></head></html>"),
  ]);
  const out = Buffer.from(injectAnalytics(page, HASH) ?? []);

  assert.deepEqual(out.subarray(19, 22), Buffer.from([0xe9, 0xc3, 0xa9]));
  assert.equal(out.length, page.length + TAG.length);
});

test("a meta policy in the head is widened to let the tracker run", () => {
  const out = inject(
    `<html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src 'self'"></head></html>`,
  );

  assert.equal(
    out,
    `<html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src 'self'; script-src 'self'; connect-src 'self'">${TAG}</head></html>`,
  );
});

test("a meta policy that already allows the tracker is not touched", () => {
  const head = `<meta content='default-src &apos;self&apos;' HTTP-EQUIV=content-security-policy>`;
  assert.equal(inject(`<head>${head}</head>`), `<head>${head}${TAG}</head>`);
});

test("allowAnalytics adds 'self' only where a directive governs the tracker", () => {
  const cases: [string, string][] = [
    // The gateway's own policy already allows it.
    [
      "default-src 'none'; script-src 'self'; connect-src 'self' https:",
      "default-src 'none'; script-src 'self'; connect-src 'self' https:",
    ],
    // Relying on the fallback: the fallback is copied, so nothing it allowed is lost.
    [
      "default-src https://cdn.example",
      "default-src https://cdn.example; script-src https://cdn.example 'self'; connect-src https://cdn.example 'self'",
    ],
    // 'none' gives way.
    ["script-src 'none'; connect-src 'none'", "script-src 'self'; connect-src 'self'"],
    // Both script directives are widened where both are written.
    [
      "script-src-elem https://a; script-src https://b",
      "script-src-elem https://a 'self'; script-src https://b 'self'",
    ],
    // A wildcard already covers the origin.
    ["script-src *; connect-src *", "script-src *; connect-src *"],
    // Nothing governs scripts at all: nothing to widen.
    ["img-src 'self'", "img-src 'self'"],
    // Several policies, as a header joins them, each widened on its own.
    ["script-src 'self', connect-src 'none'", "script-src 'self', connect-src 'self'"],
  ];

  for (const [policy, expected] of cases) {
    assert.equal(allowAnalytics(policy, HASH), expected, policy);
  }
});

test("a strict-dynamic policy trusts the tracker by its hash, and nothing else changes", () => {
  assert.equal(
    allowAnalytics("script-src 'strict-dynamic' 'nonce-x' 'unsafe-inline'; connect-src 'self'", HASH),
    `script-src 'strict-dynamic' 'nonce-x' 'unsafe-inline' '${HASH}'; connect-src 'self'`,
  );

  // And the tag carries the hash for the policy to match.
  const out = inject(`<head><meta http-equiv="content-security-policy" content="script-src 'strict-dynamic'"></head>`);
  assert.match(out ?? "", /<script type="module" src="\/\.npanel\/script\.js" integrity="sha384-abc"><\/script>/);
});

test("a hash is never added where 'unsafe-inline' would stop working", () => {
  assert.equal(allowAnalytics("script-src 'unsafe-inline'", HASH), "script-src 'unsafe-inline' 'self'");
});
