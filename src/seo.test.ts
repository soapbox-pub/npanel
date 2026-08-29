import assert from "node:assert/strict";
import { test } from "node:test";
import {
  augmentRobots,
  canonicalLink,
  canonicalPath,
  robotsNeedsSitemap,
  robotsTxt,
  sitemapXml,
  versionDate,
} from "./seo.ts";
import type { RequestOrigin } from "./serve-site.ts";

const ORIGIN: RequestOrigin = { hostname: "docs.example.com", scheme: "https" };

test("canonicalPath prefers the clean URL a site links to", () => {
  assert.equal(canonicalPath("/index.html"), "/");
  assert.equal(canonicalPath("/blog/index.html"), "/blog/");
  assert.equal(canonicalPath("/a/b/index.html"), "/a/b/");

  // What a `cleanUrls` build publishes, against what it links to.
  assert.equal(canonicalPath("/self-hosting.html"), "/self-hosting");
  assert.equal(canonicalPath("/docs/hacking.html"), "/docs/hacking");

  // A file that merely ends in the letters is not an index of anything.
  assert.equal(canonicalPath("/reindex.html"), "/reindex");

  // `.htm` has no sibling step in path resolution, so stripping it would name
  // a URL that 404s.
  assert.equal(canonicalPath("/legacy.htm"), "/legacy.htm");

  // Assets are never canonicalized, but the function still leaves them alone.
  assert.equal(canonicalPath("/app.js"), "/app.js");
});

test("canonicalLink names one spelling for every way a page is reached", () => {
  const index = { path: "/blog/index.html", status: 200 };
  const expected = `<https://docs.example.com/blog/>; rel="canonical"`;

  assert.equal(canonicalLink(ORIGIN, "/blog", index), expected);
  assert.equal(canonicalLink(ORIGIN, "/blog/", index), expected);
  assert.equal(canonicalLink(ORIGIN, "/blog/index.html", index), expected);

  const root = { path: "/index.html", status: 200 };
  assert.equal(canonicalLink(ORIGIN, "/", root), `<https://docs.example.com/>; rel="canonical"`);

  // Both spellings of a `cleanUrls` page name the clean one — including the
  // request that asked for the file, which is the direction that dedupes.
  const page = { path: "/self-hosting.html", status: 200 };
  const clean = `<https://docs.example.com/self-hosting>; rel="canonical"`;
  assert.equal(canonicalLink(ORIGIN, "/self-hosting", page), clean);
  assert.equal(canonicalLink(ORIGIN, "/self-hosting.html", page), clean);

  // `.htm` keeps its extension, since that is the only URL that serves it.
  const legacy = { path: "/legacy.htm", status: 200 };
  assert.equal(canonicalLink(ORIGIN, "/legacy.htm", legacy), `<https://docs.example.com/legacy.htm>; rel="canonical"`);
});

test("canonicalLink is withheld where it would mislead", () => {
  // The site's not-found page standing in for a missing path.
  assert.equal(canonicalLink(ORIGIN, "/nope", { path: "/404.html", status: 404 }), undefined);

  // An asset is not a page.
  assert.equal(canonicalLink(ORIGIN, "/app.js", { path: "/app.js", status: 200 }), undefined);

  // An SPA-mode fallback: the shell answers for a client route it does not
  // spell, and a canonical would call the whole app one page.
  assert.equal(canonicalLink(ORIGIN, "/users/alex", { path: "/index.html", status: 200 }), undefined);
  assert.equal(canonicalLink(ORIGIN, "/users/alex", { path: "/404.html", status: 200 }), undefined);
});

test("canonical URLs re-encode what the stored path spells out", () => {
  const file = { path: "/café/index.html", status: 200 };
  assert.equal(canonicalLink(ORIGIN, "/caf%C3%A9/", file), `<https://docs.example.com/caf%C3%A9/>; rel="canonical"`);
});

test("sitemapXml renders absolute, escaped URLs with the version's date", () => {
  const xml = sitemapXml(ORIGIN, ["/", "/blog/", "/a&b.html"], "2026-08-29");

  assert.match(xml, /^<\?xml version="1.0" encoding="UTF-8"\?>/);
  assert.match(xml, /<urlset xmlns="http:\/\/www\.sitemaps\.org\/schemas\/sitemap\/0\.9">/);
  assert.match(xml, /<url><loc>https:\/\/docs\.example\.com\/<\/loc><lastmod>2026-08-29<\/lastmod><\/url>/);
  assert.match(xml, /<loc>https:\/\/docs\.example\.com\/blog\/<\/loc>/);
  assert.match(xml, /<loc>https:\/\/docs\.example\.com\/a&amp;b\.html<\/loc>/);
});

test("versionDate is capped at when npanel saw the manifest", () => {
  // An honest manifest: its own date is the earlier one.
  assert.equal(versionDate({ created_at: 1_756_400_000, fetched_at: 1_756_500_000 }), "2025-08-28");

  // A manifest dated in the future is pinned to the fetch.
  assert.equal(versionDate({ created_at: 4_000_000_000, fetched_at: 1_756_500_000 }), "2025-08-29");

  // And one dated nonsense still renders a date.
  assert.equal(versionDate({ created_at: Number.NaN, fetched_at: 1_756_500_000 }), "2025-08-29");
  assert.equal(versionDate({ created_at: -5, fetched_at: 10 }), "1970-01-01");
});

test("robotsTxt allows everything and names the sitemap", () => {
  const robots = robotsTxt(ORIGIN);

  assert.match(robots, /User-agent: \*\nAllow: \//);
  assert.match(robots, /Sitemap: https:\/\/docs\.example\.com\/sitemap\.xml/);
});

test("robotsNeedsSitemap spots a sitemap the site already named", () => {
  // The boilerplate every nsyte build ships, and the case this exists for.
  assert.equal(robotsNeedsSitemap("User-agent: *\nAllow: /"), true);
  assert.equal(robotsNeedsSitemap(""), true);
  assert.equal(robotsNeedsSitemap("User-agent: *\nDisallow: /admin/"), true);

  // Already answered, whichever file it points at and however it is spelled.
  assert.equal(robotsNeedsSitemap("User-agent: *\n\nSitemap: https://x.example/sitemap.xml"), false);
  assert.equal(robotsNeedsSitemap("sitemap: https://x.example/sitemap-index.xml"), false);
  assert.equal(robotsNeedsSitemap("SITEMAP:https://x.example/s.xml"), false);
  assert.equal(robotsNeedsSitemap("  \tSitemap : https://x.example/s.xml"), false);

  // A site that asked not to be indexed is not argued with.
  assert.equal(robotsNeedsSitemap("User-agent: *\nDisallow: /"), false);
  assert.equal(robotsNeedsSitemap("User-agent: badbot\nDisallow: /\n"), false);

  // But a path that merely starts with a slash is an ordinary rule.
  assert.equal(robotsNeedsSitemap("User-agent: *\nDisallow: /private"), true);
});

test("augmentRobots appends one line and changes nothing else", () => {
  // The exact 22 bytes six of the deployed sites share, with no trailing
  // newline — the join must not run the directive onto `Allow: /`.
  assert.equal(
    augmentRobots("User-agent: *\nAllow: /", ORIGIN),
    "User-agent: *\nAllow: /\n\nSitemap: https://docs.example.com/sitemap.xml\n",
  );

  // Comments, blank lines and ordering are the site's; only the end changes.
  const written = "# hello\n\nUser-agent: *\nDisallow: /drafts/\nCrawl-delay: 10\n";
  assert.equal(
    augmentRobots(written, ORIGIN),
    "# hello\n\nUser-agent: *\nDisallow: /drafts/\nCrawl-delay: 10\n\nSitemap: https://docs.example.com/sitemap.xml\n",
  );

  // An empty file has no group to be separated from.
  assert.equal(augmentRobots("", ORIGIN), "Sitemap: https://docs.example.com/sitemap.xml\n");
  assert.equal(augmentRobots("\n\n", ORIGIN), "Sitemap: https://docs.example.com/sitemap.xml\n");
});

test("the appended sitemap names the host being served", () => {
  // Six hosts share one robots.txt blob, so this must follow the request and
  // not the file it came from.
  const other: RequestOrigin = { hostname: "other.example.com", scheme: "https" };

  assert.match(augmentRobots("User-agent: *", other), /Sitemap: https:\/\/other\.example\.com\/sitemap\.xml/);

  // And an onion visitor is told about the onion's own sitemap.
  const onion: RequestOrigin = { hostname: `${"a".repeat(56)}.onion`, scheme: "http" };
  assert.match(augmentRobots("User-agent: *", onion), /Sitemap: http:\/\/a{56}\.onion\/sitemap\.xml/);
});
