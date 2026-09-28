import assert from "node:assert/strict";
import { test } from "node:test";
import { MISSING_SCROLL_DEPTH, parseEventRequest } from "./request.ts";

function parse(body: unknown) {
  return parseEventRequest(typeof body === "string" ? body : JSON.stringify(body));
}

test("a pageview from the tracker is read field by field", () => {
  const result = parse({
    n: "pageview",
    v: 36,
    u: "https://www.docs.example.com/guide%20one?utm_source=ig",
    d: "docs.example.com",
    r: "https://news.ycombinator.com/",
  });

  assert.ok("event" in result);
  assert.equal(result.event.name, "pageview");
  assert.equal(result.event.pathname, "/guide one");
  assert.deepEqual(result.event.domains, ["docs.example.com"]);
  assert.equal(result.event.referrer, "https://news.ycombinator.com/");
  assert.equal(result.event.interactive, true);
  assert.equal(result.event.scrollDepth, MISSING_SCROLL_DEPTH);
});

test("the long field names Plausible accepts work too", () => {
  const result = parse({ name: "pageview", url: "https://a.example/", domain: "a.example", interactive: false });

  assert.ok("event" in result);
  assert.equal(result.event.interactive, false);
});

test("domains are split on commas, without www, and default to the page's host", () => {
  const listed = parse({ n: "pageview", u: "https://a.example/", d: "www.a.example, b.example" });
  assert.ok("event" in listed);
  assert.deepEqual(listed.event.domains, ["a.example", "b.example"]);

  const implied = parse({ n: "pageview", u: "https://www.c.example/" });
  assert.ok("event" in implied);
  assert.deepEqual(implied.event.domains, ["c.example"]);
});

test("hash routing puts the route into the path", () => {
  const result = parse({ n: "pageview", u: "https://a.example/app#/users/1", d: "a.example", h: 1 });

  assert.ok("event" in result);
  assert.equal(result.event.pathname, "/app#/users/1");
});

test("engagement carries scroll depth and time, clamped the way Plausible clamps them", () => {
  const result = parse({ n: "engagement", u: "https://a.example/", d: "a.example", sd: 140, e: "4500" });

  assert.ok("event" in result);
  assert.equal(result.event.scrollDepth, 100);
  assert.equal(result.event.engagementTime, 4500);
});

test("bad requests get Plausible's errors", () => {
  assert.deepEqual(parse("not json"), { errors: { request: ["Unable to parse request body as json"] } });
  assert.deepEqual(parse([1, 2]), { errors: { request: ["Unable to parse request body as json"] } });
  assert.deepEqual(parse({ n: "pageview", d: "a.example" }), { errors: { url: ["is required"] } });
  assert.deepEqual(parse({ n: "pageview", u: "data:text/html,hi", d: "a.example" }), {
    errors: { url: ["scheme is not allowed"] },
  });
  assert.deepEqual(parse({ n: "pageview", u: "https://a.example/", d: " " }), {
    errors: { domain: ["can't be blank"] },
  });
  assert.deepEqual(parse({ u: "https://a.example/", d: "a.example" }), { errors: { event_name: ["can't be blank"] } });
  assert.deepEqual(parse({ n: "engagement", u: "https://a.example/", d: "a.example" }), {
    errors: {
      event_name: ["engagement event requires a valid integer value for at least one of 'sd' or 'e' fields"],
    },
  });
});
