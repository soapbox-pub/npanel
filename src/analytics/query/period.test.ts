import assert from "node:assert/strict";
import { test } from "node:test";
import {
  type Compare,
  type DateRangeInput,
  type PeriodContext,
  type TimeRange,
  comparisonRange,
  mainRange,
  partialTimeLabels,
  timeLabels,
} from "./period.ts";
import { isoInZone, offsetSegments } from "./zone.ts";

/** Monday, 28 September 2026, 12:34:56 UTC. */
const NOW = Date.UTC(2026, 8, 28, 12, 34, 56) / 1000;

function ctx(timezone = "UTC", relativeDate = "2026-09-28"): PeriodContext {
  return { timezone, now: NOW, relativeDate, statsStart: "2026-01-15" };
}

function iso(range: TimeRange, timezone = "UTC"): [string, string] {
  return [isoInZone(range.first, timezone), isoInZone(range.last, timezone)];
}

test("periods resolve to Plausible's ranges", () => {
  const cases: [DateRangeInput, [string, string]][] = [
    // Full days, ending yesterday.
    ["7d", ["2026-09-21T00:00:00Z", "2026-09-27T23:59:59Z"]],
    ["30d", ["2026-08-29T00:00:00Z", "2026-09-27T23:59:59Z"]],
    // Full months, ending last month.
    ["6mo", ["2026-03-01T00:00:00Z", "2026-08-31T23:59:59Z"]],
    // This month and year, trimmed to today.
    ["month", ["2026-09-01T00:00:00Z", "2026-09-28T23:59:59Z"]],
    ["year", ["2026-01-01T00:00:00Z", "2026-09-28T23:59:59Z"]],
    // Today, trimmed to the end of this hour.
    ["day", ["2026-09-28T00:00:00Z", "2026-09-28T12:59:59Z"]],
    ["all", ["2026-01-15T00:00:00Z", "2026-09-28T23:59:59Z"]],
    [
      ["2026-05-01", "2026-05-10"],
      ["2026-05-01T00:00:00Z", "2026-05-10T23:59:59Z"],
    ],
    ["realtime", ["2026-09-28T12:29:56Z", "2026-09-28T12:35:01Z"]],
  ];

  for (const [input, expected] of cases) {
    assert.deepEqual(iso(mainRange(input, ctx(), null)), expected, JSON.stringify(input));
  }
});

test("a past month is the whole month, in the site's zone", () => {
  const range = mainRange("month", ctx("Europe/Tallinn", "2026-01-15"), null);
  assert.deepEqual(iso(range, "Europe/Tallinn"), ["2026-01-01T00:00:00+02:00", "2026-01-31T23:59:59+02:00"]);
  assert.deepEqual(iso(range), ["2025-12-31T22:00:00Z", "2026-01-31T21:59:59Z"]);
});

test("comparisons shift the part of the range that has happened", () => {
  const compare = (input: DateRangeInput, with_: Compare, match: boolean) => {
    const main = mainRange(input, ctx(), with_);
    return iso(comparisonRange(input, main, with_, match, ctx()));
  };

  // Sept 1–28 so far, against the 28 days before it.
  assert.deepEqual(compare("month", "previous_period", false), ["2026-08-04T00:00:00Z", "2026-08-31T23:59:59Z"]);
  // Starting on the same weekday: 1 Sept 2026 is a Tuesday, and so is 4 Aug.
  assert.deepEqual(compare("month", "previous_period", true), ["2026-08-04T00:00:00Z", "2026-08-31T23:59:59Z"]);
  assert.deepEqual(compare("7d", "year_over_year", false), ["2025-09-21T00:00:00Z", "2025-09-27T23:59:59Z"]);
  // 21 Sept 2025 was a Sunday; the Monday nearest after it is the 22nd.
  assert.deepEqual(compare("7d", "year_over_year", true), ["2025-09-22T00:00:00Z", "2025-09-28T23:59:59Z"]);
  // Today so far, against the same hours a week ago.
  assert.deepEqual(compare("day", "previous_period", true), ["2026-09-21T00:00:00Z", "2026-09-21T12:34:56Z"]);
  assert.deepEqual(compare("day", "previous_period", false), ["2026-09-27T00:00:00Z", "2026-09-27T12:34:56Z"]);
  assert.deepEqual(compare("7d", ["2026-01-01", "2026-01-07"], true), ["2026-01-01T00:00:00Z", "2026-01-07T23:59:59Z"]);
});

test("weeks start on Monday, except the first, which starts with the range", () => {
  const range = mainRange(["2026-09-03", "2026-09-20"], ctx(), null);
  assert.deepEqual(timeLabels("week", range, "UTC", NOW), ["2026-09-03", "2026-09-07", "2026-09-14"]);
  assert.deepEqual(partialTimeLabels(["2026-09-03", "2026-09-07", "2026-09-14"], "week", range, "UTC", NOW), [
    "2026-09-03",
  ]);
});

test("months are labelled by their first day", () => {
  const range = mainRange("6mo", ctx(), null);
  assert.deepEqual(timeLabels("month", range, "UTC", NOW), [
    "2026-03-01",
    "2026-04-01",
    "2026-05-01",
    "2026-06-01",
    "2026-07-01",
    "2026-08-01",
  ]);
});

test("today by the hour runs to the current hour", () => {
  const range = mainRange("day", ctx(), null);
  const labels = timeLabels("hour", range, "UTC", NOW);
  assert.equal(labels.length, 13);
  assert.equal(labels[0], "2026-09-28 00:00:00");
  assert.equal(labels.at(-1), "2026-09-28 12:00:00");
  assert.deepEqual(partialTimeLabels(labels, "hour", range, "UTC", NOW), ["2026-09-28 12:00:00"]);
});

test("a zone's offset changes are found to the second", () => {
  // Berlin leaves summer time at 01:00 UTC on 25 October 2026.
  const segments = offsetSegments(Date.UTC(2026, 9, 24) / 1000, Date.UTC(2026, 9, 27) / 1000, "Europe/Berlin");
  assert.deepEqual(
    segments.map(([, offset]) => offset),
    [7200, 3600],
  );
  assert.equal(segments[1][0], Date.UTC(2026, 9, 25, 1) / 1000);
});
