import assert from "node:assert/strict";
import { test } from "node:test";
import { extractCsp } from "./site-csp.ts";

function document(head: string): string {
  return `<!doctype html><html><head>${head}</head><body>hello</body></html>`;
}

test("reads the policy a document declares", () => {
  const html = document(`<meta http-equiv="Content-Security-Policy" content="default-src 'self'">`);
  assert.equal(extractCsp(html), "default-src 'self'");
});

test("http-equiv is matched however it was spelled", () => {
  const html = document(`<meta HTTP-EQUIV=" content-security-POLICY " content=" default-src 'self' ">`);
  assert.equal(extractCsp(html), "default-src 'self'");
});

test("a document that declares nothing declares nothing", () => {
  assert.equal(extractCsp(document("<title>page</title>")), null);
  assert.equal(extractCsp(document(`<meta name="description" content="default-src 'self'">`)), null);
  assert.equal(extractCsp(""), null);
});

test("a report-only policy is not the policy a site is held to", () => {
  const html = document(
    `<meta http-equiv="Content-Security-Policy-Report-Only" content="default-src 'none'; report-uri /csp">`,
  );

  assert.equal(extractCsp(html), null);
});

test("several meta tags become the comma-separated list the header defines", () => {
  // A browser enforces each meta policy on top of the last, and comma-separated
  // policies in one header behave identically. Taking only the first would
  // quietly drop a restriction the site asked for.
  const html = document(
    `<meta http-equiv="content-security-policy" content="default-src 'self'">` +
      `<meta http-equiv="content-security-policy" content="script-src 'none'">`,
  );

  assert.equal(extractCsp(html), "default-src 'self', script-src 'none'");
});

test("entities in the content attribute are decoded", () => {
  const html = document(
    `<meta http-equiv="content-security-policy" content="img-src 'self' https://a.example&#x2F;x">`,
  );
  assert.equal(extractCsp(html), "img-src 'self' https://a.example/x");
});

test("a policy that could forge a second header is refused", () => {
  // Nothing in a site's markup may decide what other headers npanel sends, and
  // `Headers.set` would throw on this rather than escape it — a 500 for the
  // site, from a stray newline in an attribute.
  const html = document(`<meta http-equiv="content-security-policy" content="default-src 'self'\r\nSet-Cookie: a=b">`);
  assert.equal(extractCsp(html), null);
});

test("an empty policy is not a policy", () => {
  assert.equal(extractCsp(document(`<meta http-equiv="content-security-policy" content="   ">`)), null);
  assert.equal(extractCsp(document(`<meta http-equiv="content-security-policy" content="">`)), null);
});

test("a policy too long to send falls back to the gateway's", () => {
  const huge = `script-src ${"'sha256-aaaa' ".repeat(1000)}`;
  assert.ok(huge.length > 8 * 1024);

  assert.equal(extractCsp(document(`<meta http-equiv="content-security-policy" content="${huge}">`)), null);
});
