import assert from "node:assert/strict";
import { test } from "node:test";
import { channel } from "./channel.ts";
import { acquisition, isSpamReferrer } from "./source.ts";

const page = (query = "") => new URL(`https://docs.example.com/guide${query}`);

test("a known referrer is named as its source, whatever domain it came from", () => {
  assert.deepEqual(acquisition(page(), "https://www.google.co.uk/"), { source: "Google", referrer: "google.co.uk" });
  assert.equal(acquisition(page(), "https://duckduckgo.com/?q=x").source, "DuckDuckGo");
  // Renamed by Plausible's own list on top of referer-parser's.
  assert.equal(acquisition(page(), "https://t.co/abc").source, "X (Twitter)");
  // Plausible's exact hosts win over referer-parser's families.
  assert.equal(acquisition(page(), "https://gemini.google.com/app").source, "Google Gemini");
  assert.equal(acquisition(page(), "https://en.wikipedia.org/wiki/Nostr").source, "Wikipedia");
});

test("an unknown referrer is its host, and the referring page is kept without its query", () => {
  assert.deepEqual(acquisition(page(), "https://blog.example.org/posts/nsites/?ref=x"), {
    source: "blog.example.org",
    referrer: "blog.example.org/posts/nsites",
  });
});

test("a tagged URL wins over the referrer, and known tags are folded into sources", () => {
  assert.equal(acquisition(page("?utm_source=ig"), "https://www.google.com/").source, "Instagram");
  assert.equal(acquisition(page("?ref=newsletter"), null).source, "newsletter");
  assert.equal(acquisition(page("?utm_source=GOOGLE"), null).source, "Google");
});

test("the site itself, localhost, and non-web referrers are direct visits", () => {
  for (const referrer of [
    "https://www.docs.example.com/other",
    "http://localhost:5173/",
    "file:///home/me/page.html",
    "not a url",
    null,
  ]) {
    assert.deepEqual(acquisition(page(), referrer), { source: "", referrer: "" }, String(referrer));
  }
});

test("an Android app is a referrer of its own", () => {
  assert.equal(acquisition(page(), "android-app://com.reddit.frontpage").source, "Reddit");
});

test("spam referrers are recognized by their domain or any subdomain of it", () => {
  assert.equal(isSpamReferrer("https://zvooq.eu/"), true);
  assert.equal(isSpamReferrer("https://www.zvooq.eu/x"), true);
  assert.equal(isSpamReferrer("https://news.ycombinator.com/"), false);
  assert.equal(isSpamReferrer(null), false);
});

test("channels follow Plausible's rules in order", () => {
  const cases: [Parameters<typeof channel>[0], string][] = [
    [{ source: "", utmMedium: "", utmCampaign: "", utmSource: "", clickIdParam: "" }, "Direct"],
    [{ source: "Google", utmMedium: "", utmCampaign: "", utmSource: "", clickIdParam: "" }, "Organic Search"],
    [{ source: "Google", utmMedium: "(gclid)", utmCampaign: "", utmSource: "", clickIdParam: "gclid" }, "Paid Search"],
    [{ source: "Facebook", utmMedium: "cpc", utmCampaign: "", utmSource: "", clickIdParam: "" }, "Paid Social"],
    [{ source: "Hacker News", utmMedium: "", utmCampaign: "", utmSource: "", clickIdParam: "" }, "Organic Social"],
    [{ source: "ChatGPT", utmMedium: "", utmCampaign: "", utmSource: "", clickIdParam: "" }, "AI Assistants"],
    [{ source: "", utmMedium: "email", utmCampaign: "", utmSource: "", clickIdParam: "" }, "Email"],
    [{ source: "", utmMedium: "banner", utmCampaign: "", utmSource: "", clickIdParam: "" }, "Display"],
    [
      { source: "", utmMedium: "", utmCampaign: "summer-cross-network", utmSource: "", clickIdParam: "" },
      "Cross-network",
    ],
    [{ source: "blog.example.org", utmMedium: "", utmCampaign: "", utmSource: "", clickIdParam: "" }, "Referral"],
  ];

  for (const [inputs, expected] of cases) {
    assert.equal(channel(inputs), expected, JSON.stringify(inputs));
  }
});
