import assert from "node:assert/strict";
import { after, test } from "node:test";
import { openSqlite } from "../../db/sqlite.ts";
import type { Geo } from "../geo.ts";
import { Ingest } from "../ingest.ts";
import { Salts } from "../salts.ts";
import { SessionCache } from "../sessions.ts";
import { AnalyticsStore } from "../store.ts";
import { UserAgents } from "../user-agent.ts";
import { QueryEngine } from "./engine.ts";
import { QueryError, parseStatsQuery } from "./parse.ts";

const stores: AnalyticsStore[] = [];

after(() => {
  for (const store of stores) store.db.close();
});

const HOST = { id: 1, hostname: "docs.example.com" };
const FIREFOX = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";
const IPHONE =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";

/** Monday, 28 September 2026, noon UTC: "now" for every test here. */
const NOW = Date.UTC(2026, 8, 28, 12) / 1000;

const at = (iso: string) => Date.parse(iso) / 1000;

/** Where each test visitor's address places them. */
const PLACES: Record<string, { country: string; region: string; city: string }> = {
  "198.51.100.1": { country: "DE", region: "Bavaria", city: "Munich" },
  "198.51.100.2": { country: "DE", region: "Berlin", city: "Berlin" },
  "198.51.100.3": { country: "US", region: "Oregon", city: "Portland" },
};

/**
 * A site with a known week of visits:
 *
 * - A, on the 27th: lands on `/` from Google, reads `/pricing` for 30s to 80%.
 * - B, on the 27th: lands on `/pricing` from Hacker News, reads 10s to 40%, leaves.
 * - C, on the 26th: lands on `/` directly, leaves.
 * - D, on the 20th — the week before — lands on `/` from Google on an iPhone.
 */
async function setup() {
  const store = new AnalyticsStore(await openSqlite(":memory:"));
  stores.push(store);

  const ingest = new Ingest({
    store,
    sessions: new SessionCache(),
    salts: new Salts(store),
    userAgents: new UserAgents(),
    geo: { lookup: (ip: string) => PLACES[ip] ?? { country: "", region: "", city: "" } } as unknown as Geo,
    host: (domain) => (domain === HOST.hostname ? { ...HOST, counting: true } : undefined),
    flush: () => {},
  });

  const send = (when: string, ip: string, fields: Record<string, unknown>, userAgent = FIREFOX) => {
    const path = (fields.path as string) ?? "/";
    const body = { n: "pageview", u: `https://docs.example.com${path}`, d: HOST.hostname, ...fields };
    const response = ingest.handle(JSON.stringify(body), { ip, userAgent }, at(when));
    assert.equal(response.status, 202);
  };

  send("2026-09-27T10:00:00Z", "198.51.100.1", { r: "https://www.google.com/" });
  send("2026-09-27T10:02:00Z", "198.51.100.1", { path: "/pricing" });
  send("2026-09-27T10:05:00Z", "198.51.100.1", { n: "engagement", path: "/pricing", e: 30_000, sd: 80 });

  send("2026-09-27T11:00:00Z", "198.51.100.2", { path: "/pricing", r: "https://news.ycombinator.com/" });
  send("2026-09-27T11:01:00Z", "198.51.100.2", { n: "engagement", path: "/pricing", e: 10_000, sd: 40 });

  send("2026-09-26T09:00:00Z", "198.51.100.3", {});

  send("2026-09-20T09:00:00Z", "198.51.100.4", { r: "https://www.google.com/" }, IPHONE);

  store.flush();

  const engine = new QueryEngine(store.db, () => NOW);
  const query = (body: Record<string, unknown>, timezone = "UTC") =>
    engine.query(HOST, parseStatsQuery({ relative_date: "2026-09-28", ...body }), timezone) as {
      results: { dimensions: unknown[]; metrics: unknown[]; comparison?: { metrics: unknown[]; change: unknown[] } }[];
      comparison_results?: { dimensions: unknown[]; metrics: unknown[]; change: unknown[] | null }[];
      meta: Record<string, unknown>;
      query: Record<string, unknown>;
    };

  return { ingest, store, engine, query, send };
}

const TOP_STATS = ["visitors", "visits", "pageviews", "views_per_visit", "bounce_rate", "visit_duration"];

test("top stats count the week's visits the way Plausible does", async () => {
  const { query } = await setup();

  const { results } = query({ date_range: "7d", metrics: TOP_STATS, dimensions: [] });

  // Three people, one visit each, four pageviews. A left after two minutes;
  // B and C bounced. Engagement is not a pageview and ends no bounce.
  assert.deepEqual(results, [{ dimensions: [], metrics: [3, 3, 4, 1.33, 67, 40] }]);
});

test("top stats compare against the week before, weekday for weekday", async () => {
  const { query } = await setup();

  const response = query({
    date_range: "7d",
    metrics: TOP_STATS,
    dimensions: [],
    include: { compare: "previous_period", compare_match_day_of_week: true },
  });

  assert.deepEqual(response.results[0].comparison, {
    dimensions: [],
    metrics: [1, 1, 1, 1, 100, 0],
    // Bounce rate changes by points; everything else by percent.
    change: [200, 200, 300, 33, -33, 100],
  });
  assert.deepEqual(response.query.date_range, ["2026-09-21T00:00:00Z", "2026-09-27T23:59:59Z"]);
  assert.deepEqual(response.query.comparison_date_range, ["2026-09-14T00:00:00Z", "2026-09-20T23:59:59Z"]);
});

test("the main graph has a label for every day, and results only for the days with visits", async () => {
  const { query } = await setup();

  const { results, meta } = query({
    date_range: "7d",
    metrics: ["visitors"],
    dimensions: ["time:day"],
    include: { time_labels: true, partial_time_labels: true, present_index: true, empty_metrics: true },
  });

  assert.deepEqual(results, [
    { dimensions: ["2026-09-26"], metrics: [1] },
    { dimensions: ["2026-09-27"], metrics: [2] },
  ]);
  assert.deepEqual(meta, {
    empty_metrics: [0],
    partial_time_labels: [],
    present_index: null,
    time_label_result_indices: [null, null, null, null, null, 0, 1],
    time_labels: ["2026-09-21", "2026-09-22", "2026-09-23", "2026-09-24", "2026-09-25", "2026-09-26", "2026-09-27"],
  });
});

test("this month ends today, and today is the partial bucket", async () => {
  const { query } = await setup();

  const { meta } = query({
    date_range: "month",
    metrics: ["visitors"],
    dimensions: ["time:day"],
    include: { time_labels: true, partial_time_labels: true, present_index: true },
  });

  const labels = meta.time_labels as string[];
  assert.equal(labels.length, 28);
  assert.equal(labels.at(-1), "2026-09-28");
  assert.equal(meta.present_index, 27);
  assert.deepEqual(meta.partial_time_labels, ["2026-09-28"]);
});

test("a time series comparison is a list of its own, lined up bucket by bucket", async () => {
  const { query } = await setup();

  const response = query({
    date_range: "7d",
    metrics: ["visitors"],
    dimensions: ["time:day"],
    include: { time_labels: true, compare: "previous_period", compare_match_day_of_week: true },
  });

  // D's visit on the 20th lines up with the 27th, which had two.
  assert.deepEqual(response.comparison_results, [{ dimensions: ["2026-09-20"], metrics: [1], change: [100] }]);
  assert.deepEqual((response.meta.comparison_time_labels as string[]).slice(-1), ["2026-09-20"]);
  assert.deepEqual(response.meta.comparison_time_label_result_indices, [null, null, null, null, null, null, 0]);
});

test("sources are named, blanks are Direct / None, and each row knows its share", async () => {
  const { query } = await setup();

  const { results } = query({
    date_range: "7d",
    metrics: ["visitors", "percentage"],
    dimensions: ["visit:source"],
    order_by: [
      ["visitors", "desc"],
      ["visit:source", "asc"],
    ],
  });

  assert.deepEqual(results, [
    { dimensions: ["Direct / None"], metrics: [1, 33.33] },
    { dimensions: ["Google"], metrics: [1, 33.33] },
    { dimensions: ["Hacker News"], metrics: [1, 33.33] },
  ]);
});

test("pages are counted by pageviews, and a page's bounce rate is of the visits that landed on it", async () => {
  const { query } = await setup();

  const { results } = query({
    date_range: "7d",
    metrics: ["visitors", "pageviews", "bounce_rate", "time_on_page", "scroll_depth"],
    dimensions: ["event:page"],
    order_by: [["event:page", "asc"]],
  });

  assert.deepEqual(results, [
    // A and C viewed it; of the two who landed here, C bounced. No engagement.
    { dimensions: ["/"], metrics: [2, 2, 50, null, null] },
    // A and B viewed it; only B landed here, and bounced. 40s over two visits,
    // and they scrolled to 80% and 40%.
    { dimensions: ["/pricing"], metrics: [2, 2, 100, 20, 60] },
  ]);
});

test("a page filter narrows visitors to the page, and the bounce rate to visits that entered on it", async () => {
  const { query } = await setup();

  const { results } = query({
    date_range: "7d",
    metrics: ["visitors", "visits", "pageviews", "bounce_rate", "time_on_page", "scroll_depth"],
    filters: [["is", "event:page", ["/pricing"]]],
  });

  assert.deepEqual(results, [{ dimensions: [], metrics: [2, 2, 2, 100, 20, 60] }]);
});

test("filters apply by value, by substring, negated, and without case", async () => {
  const { query } = await setup();
  const visitors = (filters: unknown[]) =>
    query({ date_range: "7d", metrics: ["visitors"], filters }).results[0].metrics[0];

  assert.equal(visitors([["is", "visit:source", ["Google"]]]), 1);
  assert.equal(visitors([["is", "visit:source", ["Direct / None"]]]), 1);
  assert.equal(visitors([["is_not", "visit:source", ["Google"]]]), 2);
  assert.equal(visitors([["contains", "visit:source", ["oo"]]]), 1);
  assert.equal(visitors([["contains", "visit:source", ["google"]]]), 0);
  assert.equal(visitors([["contains", "visit:source", ["google"], { case_sensitive: false }]]), 1);
  assert.equal(visitors([["contains_not", "event:page", ["pric"]]]), 2);
  // Pages by where their visitors came from.
  const pages = query({
    date_range: "7d",
    metrics: ["visitors"],
    dimensions: ["event:page"],
    filters: [["is", "visit:source", ["Hacker News"]]],
  }).results;
  assert.deepEqual(pages, [{ dimensions: ["/pricing"], metrics: [1] }]);
});

test("entry and exit pages count visits, and exit rate is exits over the page's pageviews", async () => {
  const { query } = await setup();

  const entries = query({
    date_range: "7d",
    metrics: ["visitors", "visits"],
    dimensions: ["visit:entry_page"],
    order_by: [["visit:entry_page", "asc"]],
  }).results;
  assert.deepEqual(entries, [
    { dimensions: ["/"], metrics: [2, 2] },
    { dimensions: ["/pricing"], metrics: [1, 1] },
  ]);

  const exits = query({
    date_range: "7d",
    metrics: ["visits", "exit_rate"],
    dimensions: ["visit:exit_page"],
    order_by: [["visit:exit_page", "asc"]],
  }).results;
  assert.deepEqual(exits, [
    { dimensions: ["/"], metrics: [1, 50] },
    { dimensions: ["/pricing"], metrics: [2, 100] },
  ]);
});

test("locations come with their names, and the dashboard's no-country filter works", async () => {
  const { query } = await setup();

  const countries = query({
    date_range: "7d",
    metrics: ["visitors"],
    dimensions: ["visit:country_name", "visit:country"],
    filters: [["is_not", "visit:country", ["\u0000\u0000", "ZZ"]]],
    order_by: [["visitors", "desc"]],
  }).results;
  assert.deepEqual(countries, [
    { dimensions: ["Germany", "DE"], metrics: [2] },
    { dimensions: ["United States", "US"], metrics: [1] },
  ]);

  const cities = query({
    date_range: "7d",
    metrics: ["visitors"],
    dimensions: ["visit:city_name", "visit:city", "visit:country"],
    filters: [["is", "visit:region", ["DE-Bavaria"]]],
  }).results;
  assert.deepEqual(cities, [{ dimensions: ["Munich", "DE-Bavaria-Munich", "DE"], metrics: [1] }]);
});

test("devices are Plausible's names and screen sizes", async () => {
  const { query } = await setup();

  const browsers = query({
    date_range: "28d",
    metrics: ["visitors"],
    dimensions: ["visit:browser"],
    order_by: [["visitors", "desc"]],
  }).results;
  assert.deepEqual(browsers, [
    { dimensions: ["Firefox"], metrics: [3] },
    { dimensions: ["Safari"], metrics: [1] },
  ]);

  const screens = query({ date_range: "28d", metrics: ["visitors"], dimensions: ["visit:device"] }).results;
  assert.deepEqual(new Set(screens.map((row) => row.dimensions[0])), new Set(["Desktop", "Mobile"]));
});

test("days are the site's days, in the instance's time zone", async () => {
  const { query } = await setup();

  // C's visit at 09:00 UTC on the 26th is still the 26th in Los Angeles, and
  // A's and B's are on the 27th — but a visit at 02:00 UTC would be the day before.
  const la = query(
    { date_range: "7d", metrics: ["visitors"], dimensions: ["time:day"], include: { time_labels: true } },
    "America/Los_Angeles",
  );
  assert.deepEqual(la.query.date_range, ["2026-09-21T00:00:00-07:00", "2026-09-27T23:59:59-07:00"]);
  assert.deepEqual(la.results, [
    { dimensions: ["2026-09-26"], metrics: [1] },
    { dimensions: ["2026-09-27"], metrics: [2] },
  ]);

  const tokyo = query({ date_range: "7d", metrics: ["visitors"], dimensions: ["time:day"] }, "Asia/Tokyo");
  // 10:00 and 11:00 UTC on the 27th are 19:00 and 20:00 in Tokyo, the same day.
  assert.deepEqual(tokyo.results.at(-1), { dimensions: ["2026-09-27"], metrics: [2] });
});

test("realtime counts who is here now, and the last half hour by the minute", async () => {
  const { query, send, store, engine } = await setup();
  send("2026-09-28T11:59:00Z", "198.51.100.9", { path: "/now" });
  store.flush();

  assert.equal(engine.currentVisitors(HOST), 1);

  const { results, meta } = query({
    date_range: "realtime_30m",
    metrics: ["visitors", "pageviews"],
    dimensions: ["time:minute"],
    include: { time_labels: true },
  });
  assert.deepEqual(results, [{ dimensions: ["2026-09-28 11:59:00"], metrics: [1, 1] }]);
  assert.equal((meta.time_labels as string[]).length, 30);
  assert.equal((meta.time_labels as string[]).at(-1), "2026-09-28 11:59:00");
});

test("filter suggestions list the values seen, most visited first", async () => {
  const { engine } = await setup();
  const range = { dateRange: "7d" as const, relativeDate: "2026-09-28", filters: [] };

  assert.deepEqual(
    engine
      .suggestions(HOST, "visit:source", range, "", "UTC")
      .map((option) => option.value)
      .sort(),
    ["Direct / None", "Google", "Hacker News"],
  );
  assert.deepEqual(engine.suggestions(HOST, "event:page", range, "pri", "UTC"), [
    { value: "/pricing", label: "/pricing" },
  ]);
  assert.deepEqual(engine.suggestions(HOST, "visit:country", range, "", "UTC"), [
    { value: "DE", label: "Germany" },
    { value: "US", label: "United States" },
  ]);
});

test("questions Plausible refuses are refused with its reasons", () => {
  const refuse = (body: Record<string, unknown>, message: RegExp) =>
    assert.throws(
      () => parseStatsQuery({ date_range: "7d", ...body }),
      (error: Error) => {
        assert.ok(error instanceof QueryError);
        assert.match(error.message, message);
        return true;
      },
    );

  refuse({ metrics: ["goals"] }, /Unknown metric 'goals'/);
  refuse({ metrics: [] }, /At least one metric/);
  refuse({ metrics: ["time_on_page"] }, /event:page/);
  refuse({ metrics: ["views_per_visit"], dimensions: ["visit:source"] }, /views_per_visit/);
  refuse({ metrics: ["visitors"], filters: [["matches", "event:page", ["/"]]] }, /operator 'matches'/);
  refuse({ metrics: ["visitors"], date_range: "all", include: { compare: "previous_period" } }, /Comparisons/);
});
