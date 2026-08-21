import assert from "node:assert/strict";
import { test } from "node:test";
import { isOnionAddress, isRootDomain, parseHostname, recordName, requestHostname, wwwApex } from "./hostname.ts";

test("requestHostname reads Host, and nothing else", () => {
  const hostname = (headers: Record<string, string>) => requestHostname(new Request("http://127.0.0.1/", { headers }));

  assert.equal(hostname({ host: "Docs.Example.COM" }), "docs.example.com");
  assert.equal(hostname({ host: "docs.example.com:8443" }), "docs.example.com");
  // A fully-qualified name with a port has both to strip, in that order.
  assert.equal(hostname({ host: "docs.example.com.:8443" }), "docs.example.com");
  assert.equal(hostname({}), "");

  // The one header a client on either listener could have written itself.
  assert.equal(hostname({ host: "docs.example.com", "x-forwarded-host": "panel.example.com" }), "docs.example.com");
});

test("parseHostname normalizes and rejects what isn't a domain", () => {
  assert.equal(parseHostname("  Docs.Example.COM. ", []), "docs.example.com");

  assert.throws(() => parseHostname("", []), /Enter a domain/);
  assert.throws(() => parseHostname("https://example.com", []), /Not a valid domain/);
  assert.throws(() => parseHostname("*.example.com", []), /Not a valid domain/);
});

test("parseHostname holds hostnames to the allowed suffixes", () => {
  assert.equal(parseHostname("docs.example.com", ["example.com"]), "docs.example.com");
  assert.equal(parseHostname("example.com", ["example.com"]), "example.com", "the suffix itself");

  assert.throws(() => parseHostname("example.org", ["example.com"]), /outside the domains/);
});

test("isRootDomain follows the public suffix list, not the label count", () => {
  assert.equal(isRootDomain("example.com"), true);
  assert.equal(isRootDomain("docs.example.com"), false);
  assert.equal(isRootDomain("www.example.com"), false);

  // Three labels, and a root domain: `co.uk` is where the registry ends.
  assert.equal(isRootDomain("example.co.uk"), true);
  assert.equal(isRootDomain("foo.example.co.uk"), false);

  // A suffix nobody registers under is not a domain anyone owns.
  assert.equal(isRootDomain("localhost"), false);
  assert.equal(isRootDomain("1.2.3.4"), false);
});

test("wwwApex names the domain a www hostname belongs to", () => {
  assert.equal(wwwApex("www.example.com"), "example.com");
  assert.equal(wwwApex("www.example.co.uk"), "example.co.uk");

  assert.equal(wwwApex("example.com"), undefined);
  // The apex of this is `example.com`, not the label in front of it, so npanel
  // has no business folding it anywhere.
  assert.equal(wwwApex("www.docs.example.com"), undefined);
  assert.equal(wwwApex("wwwexample.com"), undefined);
});

test("recordName says what to type in a DNS provider's name field", () => {
  assert.equal(recordName("example.com"), "@");
  assert.equal(recordName("example.co.uk"), "@");
  assert.equal(recordName("docs.example.com"), "docs");
  assert.equal(recordName("a.b.example.com"), "a.b");

  assert.equal(recordName("localhost"), undefined);
});

test("isOnionAddress matches a v3 address and nothing else", () => {
  const v3 = "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion";
  assert.equal(isOnionAddress(v3), true);

  assert.equal(isOnionAddress("example.com"), false);
  // v2 addresses were 16 characters. Tor dropped them in 2021.
  assert.equal(isOnionAddress("expyuzz4wqqyqhjn.onion"), false);
  // `1` and `8` are not in base32's alphabet.
  assert.equal(isOnionAddress(`${"1".repeat(56)}.onion`), false);
  assert.equal(isOnionAddress(`www.${v3}`), false);
});

test("onion addresses are npanel's to hand out, not staff's to type", () => {
  const v3 = "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion";

  assert.throws(() => parseHostname(v3, []), /npanel gives each site its own/);
  // A mistyped one gets the same sentence, rather than being read as a domain
  // under an unusual TLD and rejected for some unrelated reason.
  assert.throws(() => parseHostname("mysite.onion", []), /npanel gives each site its own/);
});

test("an onion address is not a domain with a www in front of it", () => {
  const v3 = "vww6ybal4bd7szmgncyruucpgfkqahzddi37ktceo3ah7ngmcopnpyyd.onion";

  // `onion` is in the public suffix list, so without a guard tldts reads this
  // as an apex and npanel would offer a www redirect into nothing.
  assert.equal(isRootDomain(v3), false);
  assert.equal(wwwApex(`www.${v3}`), undefined);
  assert.equal(recordName(v3), undefined);
});
