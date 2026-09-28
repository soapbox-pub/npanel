import assert from "node:assert/strict";
import { after, test } from "node:test";
import { openSqlite } from "../db/sqlite.ts";
import { Logger } from "../log.ts";
import { Geo } from "./geo.ts";
import { type AnalyticsHost, Ingest } from "./ingest.ts";
import { Salts } from "./salts.ts";
import { SessionCache } from "./sessions.ts";
import { AnalyticsStore } from "./store.ts";
import { UserAgents } from "./user-agent.ts";

const stores: AnalyticsStore[] = [];

after(() => {
  for (const store of stores) store.db.close();
});

const FIREFOX = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";
const IPHONE =
  "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";
const GOOGLEBOT = "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)";

/** Noon on an ordinary day, in epoch seconds. */
const NOON = Date.UTC(2026, 8, 1, 12) / 1000;

const HOSTS: AnalyticsHost[] = [
  { id: 1, hostname: "docs.example.com", counting: true },
  { id: 2, hostname: "quiet.example.com", counting: false },
  { id: 3, hostname: "www.shop.example.com", counting: true },
];

async function setup() {
  const store = new AnalyticsStore(await openSqlite(":memory:"));
  stores.push(store);
  const hosts = new Map(HOSTS.map((host) => [host.hostname, host]));

  const ingest = new Ingest({
    store,
    sessions: new SessionCache(),
    salts: new Salts(store),
    userAgents: new UserAgents(),
    geo: new Geo("/nonexistent", new Logger("error")),
    host: (domain) => hosts.get(domain) ?? hosts.get(`www.${domain}`),
    flush: () => store.flush(),
  });

  /** Send an event as a visitor, and flush it to the database. */
  const send = (fields: Record<string, unknown>, opts: { at?: number; ip?: string; userAgent?: string } = {}) => {
    const response = ingest.handle(
      JSON.stringify({ n: "pageview", u: "https://docs.example.com/", d: "docs.example.com", ...fields }),
      { ip: opts.ip ?? "203.0.113.7", userAgent: opts.userAgent ?? FIREFOX },
      opts.at ?? NOON,
    );
    store.flush();
    return response;
  };

  const sessions = () => store.db.prepare("SELECT * FROM sessions ORDER BY start").all() as Record<string, unknown>[];
  const events = () => store.db.prepare("SELECT * FROM events ORDER BY timestamp").all() as Record<string, unknown>[];

  return { store, send, sessions, events };
}

test("a pageview starts a visit that records where it came from and on what", async () => {
  const { send, sessions, events } = await setup();

  const response = send(
    { u: "https://docs.example.com/guide?utm_medium=social", r: "https://news.ycombinator.com/item?id=1" },
    { userAgent: IPHONE },
  );

  assert.deepEqual(response, { status: 202, headers: { "content-type": "text/plain; charset=utf-8" }, body: "ok" });

  const [session] = sessions();
  assert.equal(session.host_id, 1);
  assert.equal(session.entry_page, "/guide");
  assert.equal(session.exit_page, "/guide");
  assert.equal(session.pageviews, 1);
  assert.equal(session.is_bounce, 1);
  assert.equal(session.source, "Hacker News");
  assert.equal(session.referrer, "news.ycombinator.com/item");
  assert.equal(session.channel, "Organic Social");
  assert.equal(session.utm_medium, "social");
  assert.equal(session.browser, "Safari");
  assert.equal(session.os, "iOS");
  assert.equal(session.device, "Mobile");

  const [event] = events();
  assert.equal(event.kind, 0);
  assert.equal(event.session_id, session.session_id);
  assert.equal(event.pathname, "/guide");
});

test("a second pageview continues the visit and ends the bounce", async () => {
  const { send, sessions } = await setup();

  send({ u: "https://docs.example.com/" });
  send({ u: "https://docs.example.com/pricing" }, { at: NOON + 90 });

  const [session] = sessions();
  assert.equal(sessions().length, 1);
  assert.equal(session.pageviews, 2);
  assert.equal(session.events, 2);
  assert.equal(session.is_bounce, 0);
  assert.equal(session.entry_page, "/");
  assert.equal(session.exit_page, "/pricing");
  assert.equal(session.duration, 90);
});

test("half an hour of nothing ends a visit", async () => {
  const { send, sessions } = await setup();

  send({});
  send({}, { at: NOON + 30 * 60 });
  assert.equal(sessions().length, 1, "thirty minutes still counts");

  send({}, { at: NOON + 30 * 60 + 31 * 60 });
  assert.equal(sessions().length, 2);
});

test("engagement continues a visit without counting as anything", async () => {
  const { send, sessions, events } = await setup();

  // With no visit to belong to, it is dropped.
  const orphan = send({ n: "engagement", sd: 40, e: 5000 });
  assert.equal(orphan.headers["x-plausible-dropped"], "1");
  assert.equal(events().length, 0);

  send({});
  send({ n: "engagement", sd: 80, e: 12000 }, { at: NOON + 20 });

  const [session] = sessions();
  assert.equal(session.events, 1, "engagement is not an event the visit counts");
  assert.equal(session.is_bounce, 1, "reading a page is not leaving the bounce");

  const engagement = events().find((event) => event.kind === 1);
  assert.equal(engagement?.scroll_depth, 80);
  assert.equal(engagement?.engagement_time, 12000);
  assert.equal(engagement?.session_id, session.session_id);
});

test("the same person on another day is another visitor", async () => {
  const { send, sessions } = await setup();

  send({}, { at: NOON });
  send({}, { at: NOON + 24 * 60 * 60 });

  const [first, second] = sessions();
  assert.notEqual(first.user_id, second.user_id);
});

test("a visit in progress at midnight carries on across it", async () => {
  const { send, sessions } = await setup();
  const midnight = NOON + 12 * 60 * 60;

  send({}, { at: midnight - 60 });
  send({ u: "https://docs.example.com/next" }, { at: midnight + 60 });

  assert.equal(sessions().length, 1);
  assert.equal(sessions()[0].pageviews, 2);
});

test("different people are different visitors, and nothing identifying is stored", async () => {
  const { send, sessions, store } = await setup();

  send({}, { ip: "203.0.113.7" });
  send({}, { ip: "198.51.100.9" });

  assert.equal(sessions().length, 2);

  const dump = JSON.stringify(store.db.prepare("SELECT * FROM sessions").all());
  assert.doesNotMatch(dump, /203\.0\.113\.7|198\.51\.100\.9|Firefox\/130/);
});

test("bots, sites that aren't counting, and spam are dropped with a 202", async () => {
  const { send, events } = await setup();

  assert.equal(send({}, { userAgent: GOOGLEBOT }).headers["x-plausible-dropped"], "1");
  assert.equal(send({ d: "quiet.example.com" }).headers["x-plausible-dropped"], "1");
  assert.equal(send({ d: "nobody.example.com" }).headers["x-plausible-dropped"], "1");
  assert.equal(send({ r: "https://zvooq.eu/" }).headers["x-plausible-dropped"], "1");

  assert.equal(events().length, 0);
});

test("one event may be recorded for several sites at once", async () => {
  const { send, sessions } = await setup();

  // `d` never carries www, so a site configured under its www name is found without it.
  const response = send({ d: "docs.example.com,shop.example.com,quiet.example.com" });

  assert.equal(response.headers["x-plausible-dropped"], "1");
  assert.deepEqual(
    sessions().map((session) => session.host_id),
    [1, 3],
  );
});

test("a malformed event is a 400 with Plausible's error body", async () => {
  const { store } = await setup();
  const ingest = new Ingest({
    store,
    sessions: new SessionCache(),
    salts: new Salts(store),
    userAgents: new UserAgents(),
    geo: new Geo("/nonexistent", new Logger("error")),
    host: () => undefined,
    flush: () => {},
  });

  const response = ingest.handle("{}", { ip: "", userAgent: FIREFOX }, NOON);
  assert.equal(response.status, 400);
  assert.deepEqual(JSON.parse(response.body), {
    errors: { event_name: ["can't be blank"], url: ["is required"], domain: ["can't be blank"] },
  });
});

test("a restart picks up the visits still open", async () => {
  const { send, store } = await setup();
  send({});

  const open = store.openSessions(NOON - 60);
  assert.equal(open.length, 1);
  assert.equal(open[0].entryPage, "/");
  assert.equal(open[0].isBounce, true);
});
