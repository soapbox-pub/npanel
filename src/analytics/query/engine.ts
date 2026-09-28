import type { SqliteDatabase, SqliteValue } from "../../db/sqlite.ts";
import {
  type Dimension,
  type Filter,
  type Metric,
  type StatsQuery,
  type VisitDimension,
  timeInterval,
} from "./parse.ts";
import {
  type Interval,
  type PeriodContext,
  type TimeRange,
  bucketSpans,
  comparisonRange,
  dateRange,
  mainRange,
  partialTimeLabels,
  presentIndex,
  timeLabels,
} from "./period.ts";
import { isoInZone, offsetSegments, zonedDate } from "./zone.ts";

/**
 * Plausible's stats queries, answered from SQLite.
 *
 * A port of the semantics of Plausible's query engine rather than of its SQL,
 * which is ClickHouse's. What it keeps is what the numbers mean:
 *
 * - A visit belongs to a range if it was active in it, and is bucketed by when
 *   it was last active — or, by the hour and minute, counted in every bucket
 *   it was active in, so a graph of the last half hour shows everyone
 *   currently reading.
 * - Visitors and visits are counted from pageviews when the question is about
 *   pages (a page dimension or filter), and from visits otherwise.
 * - A visit-level metric broken down or filtered by page is about the visits
 *   that *entered* on that page: a page's bounce rate is the bounce rate of
 *   visits that landed there.
 * - Time on page and scroll depth come from engagement events, averaged over
 *   the visits that sent them.
 *
 * Each question becomes at most a few grouped queries — one over events, one
 * over visits, and the odd special one — whose rows are merged, ordered and
 * paginated here. The results come back in Plausible's response shape, meta
 * and all, so the dashboard reads them as it would read Plausible's.
 */

/** A site the analytics thread knows, as the answer names it. */
export interface QueryHost {
  id: number;
  hostname: string;
}

type Value = string | number | null;

interface Row {
  dimensions: Value[];
  metrics: Value[];
}

/** What one grouped query returns per group, before metrics are finished. */
type Group = Record<string, number | null>;

const COUNTRY_NAMES = new Intl.DisplayNames(["en"], { type: "region" });

/**
 * How events reach the visit they belong to: the join, and its conditions,
 * which go in the `WHERE`.
 *
 * Spelled out rather than left to the planner, which gets it catastrophically
 * wrong: events are scanned by site and time, and each looks up its one visit
 * by id. `CROSS JOIN` is SQLite's way of fixing that order, and `INDEXED BY`
 * of making the lookup use the index on the id — left to itself, the planner
 * scans every visit of the site once per event, which on a busy site never
 * finishes.
 */
const VISITS_JOIN = "CROSS JOIN sessions s INDEXED BY sessions_session_idx";
const VISITS_ON = ["s.host_id = e.host_id", "s.session_id = e.session_id"];

/** A visit longer than this is assumed not to exist, which bounds how far back a range has to look for visits. */
const LONGEST_VISIT = 86_400;

/**
 * Each visit dimension as an expression over `sessions s`, with the blank
 * values spelled the way Plausible spells them. Filters match against the same
 * expressions, which is what lets the dashboard filter by `Direct / None`.
 */
const VISIT_SQL: Record<VisitDimension, string> = {
  "visit:source": notSet("s.source", "Direct / None"),
  "visit:referrer": notSet("s.referrer", "Direct / None"),
  "visit:channel": notSet("s.channel", "Direct"),
  "visit:utm_medium": notSet("s.utm_medium", "(not set)"),
  "visit:utm_source": notSet("s.utm_source", "(not set)"),
  "visit:utm_campaign": notSet("s.utm_campaign", "(not set)"),
  "visit:utm_content": notSet("s.utm_content", "(not set)"),
  "visit:utm_term": notSet("s.utm_term", "(not set)"),
  "visit:country": "s.country",
  "visit:country_name": "s.country",
  // Regions and cities are known by name here rather than by ISO code or
  // GeoNames id, which DB-IP's free database doesn't carry. Their values are
  // made unique by prefixing what contains them.
  "visit:region": "CASE WHEN s.region = '' THEN '' ELSE s.country || '-' || s.region END",
  "visit:region_name": "s.region",
  "visit:city": "CASE WHEN s.city = '' THEN '' ELSE s.country || '-' || s.region || '-' || s.city END",
  "visit:city_name": "s.city",
  "visit:device": notSet("s.device", "(not set)"),
  "visit:browser": notSet("s.browser", "(not set)"),
  "visit:browser_version": notSet("s.browser_version", "(not set)"),
  "visit:os": notSet("s.os", "(not set)"),
  "visit:os_version": notSet("s.os_version", "(not set)"),
  "visit:entry_page": "s.entry_page",
  "visit:exit_page": "s.exit_page",
};

/** Collects the parameters of a statement as it is written. */
class Params {
  readonly values: SqliteValue[] = [];

  add(value: SqliteValue): string {
    this.values.push(value);
    return "?";
  }
}

/** Everything one grouped query needs to know. */
interface Scope {
  host: QueryHost;
  range: TimeRange;
  timezone: string;
  interval: Interval | undefined;
  /** The time buckets, for spreading visits across the hours or minutes they spanned. */
  labels: string[];
  dimensions: Dimension[];
  filters: Filter[];
}

export class QueryEngine {
  constructor(
    private readonly db: SqliteDatabase,
    private readonly now: () => number = () => Math.floor(Date.now() / 1000),
  ) {}

  /** Answer one dashboard query for one site, in Plausible's response shape. */
  query(host: QueryHost, query: StatsQuery, timezone: string): Record<string, unknown> {
    const now = this.now();
    const ctx = this.context(host, query, timezone, now);
    const interval = timeInterval(query);
    const compare = query.include.compare;

    const range = mainRange(query.dateRange, ctx, compare);
    const labels = interval ? timeLabels(interval, range, timezone, now) : [];
    const orderBy = query.orderBy ?? defaultOrder(query);

    const all = sortRows(this.rows(query, { ...this.scope(host, query, range, timezone), labels }), query, orderBy);
    const { offset = 0, limit = 10_000 } = query.pagination ?? {};
    const results: (Row & { comparison?: unknown })[] = all.slice(offset, offset + limit);

    const meta: Record<string, unknown> = {};
    if (query.include.imports || query.include.importsMeta) meta.imports_included = false;
    if (query.include.emptyMetrics) meta.empty_metrics = query.metrics.map(emptyValue);

    let comparison: TimeRange | undefined;
    let comparisonResults: (Row & { change: Value[] | null })[] | undefined;

    if (compare) {
      comparison = comparisonRange(query.dateRange, range, compare, query.include.compareMatchDayOfWeek, ctx);
      const comparisonLabels = interval ? timeLabels(interval, comparison, timezone, now) : [];
      const compared = this.rows(query, {
        ...this.scope(host, query, comparison, timezone),
        labels: comparisonLabels,
      });

      if (interval && query.dimensions.length === 1) {
        // A time series: two lists, each only its non-empty buckets, with each
        // comparison bucket's change measured against the bucket it lines up with.
        const byLabel = new Map(results.map((row) => [row.dimensions[0], row.metrics]));
        const lineUp = new Map(comparisonLabels.map((label, index) => [label, labels[index]]));

        comparisonResults = sortRows(compared, query, orderBy).map((row) => {
          const main = byLabel.get(lineUp.get(String(row.dimensions[0])) ?? "");
          return { ...row, change: main ? changes(query.metrics, row.metrics, main) : null };
        });

        if (query.include.timeLabels) {
          meta.comparison_time_labels = comparisonLabels;
          meta.comparison_time_label_result_indices = resultIndices(comparisonLabels, comparisonResults);
          if (query.include.partialTimeLabels) {
            meta.comparison_partial_time_labels = partialTimeLabels(
              comparisonLabels,
              interval,
              comparison,
              timezone,
              now,
            );
          }
        }
      } else {
        const byKey = new Map(compared.map((row) => [key(row.dimensions), row.metrics]));
        for (const row of results) {
          const metrics = byKey.get(key(row.dimensions)) ?? query.metrics.map(emptyValue);
          row.comparison = {
            dimensions: row.dimensions,
            metrics,
            change: changes(query.metrics, metrics, row.metrics),
          };
        }
      }
    }

    if (interval && query.include.timeLabels) {
      meta.time_labels = labels;
      meta.time_label_result_indices = resultIndices(labels, results);
      if (query.include.presentIndex) meta.present_index = presentIndex(labels, interval, range, timezone, now);
      if (query.include.partialTimeLabels) {
        meta.partial_time_labels = partialTimeLabels(labels, interval, range, timezone, now);
      }
    }

    const response: Record<string, unknown> = { results };
    if (comparisonResults) response.comparison_results = comparisonResults;
    response.meta = Object.fromEntries(Object.entries(meta).sort(([a], [b]) => a.localeCompare(b)));
    response.query = echo(host, query, range, comparison, orderBy, timezone);

    return response;
  }

  /** Visitors seen on a site in the last five minutes, as the dashboard's live counter shows. */
  currentVisitors(host: QueryHost): number {
    const [row] = this.db
      .prepare(
        "SELECT COUNT(DISTINCT user_id) AS visitors FROM events WHERE host_id = ? AND timestamp >= ? AND kind <> 1",
      )
      .all(host.id, this.now() - 5 * 60) as { visitors: number }[];

    return row?.visitors ?? 0;
  }

  /**
   * Values of one dimension a filter could match, most visited first, for the
   * dashboard's filter autocomplete.
   */
  suggestions(
    host: QueryHost,
    dimension: Dimension,
    query: Pick<StatsQuery, "dateRange" | "relativeDate" | "filters">,
    search: string,
    timezone: string,
  ): { value: string; label: string }[] {
    const labelled = LABELS[dimension];
    const dimensions: Dimension[] = labelled ? [dimension, labelled] : [dimension];
    const full: StatsQuery = {
      ...query,
      metrics: ["visitors"],
      dimensions,
      rawFilters: [],
      orderBy: [["visitors", "desc"]],
      pagination: null,
      include: {
        imports: false,
        importsMeta: false,
        timeLabels: false,
        partialTimeLabels: false,
        compare: null,
        compareMatchDayOfWeek: false,
        emptyMetrics: false,
        presentIndex: false,
      },
    };

    const now = this.now();
    const range = mainRange(full.dateRange, this.context(host, full, timezone, now), null);
    const needle = search.trim().toLowerCase();

    return sortRows(
      this.rows(full, { ...this.scope(host, full, range, timezone), labels: [] }),
      full,
      full.orderBy ?? [],
    )
      .map((row) => ({ value: String(row.dimensions[0] ?? ""), label: String(row.dimensions.at(-1) ?? "") }))
      .filter((option) => option.value !== "" && option.label !== "")
      .filter((option) => !needle || option.label.toLowerCase().includes(needle))
      .slice(0, 25);
  }

  private context(
    host: QueryHost,
    query: Pick<StatsQuery, "dateRange" | "relativeDate">,
    timezone: string,
    now: number,
  ) {
    const ctx: PeriodContext = {
      timezone,
      now,
      relativeDate: query.relativeDate ?? zonedDate(now, timezone),
      statsStart: undefined,
    };

    if (query.dateRange === "all") {
      const [row] = this.db.prepare("SELECT MIN(start) AS start FROM sessions WHERE host_id = ?").all(host.id) as {
        start: number | null;
      }[];
      if (row?.start != null) ctx.statsStart = zonedDate(row.start, timezone);
    }

    return ctx;
  }

  private scope(host: QueryHost, query: StatsQuery, range: TimeRange, timezone: string): Omit<Scope, "labels"> {
    return {
      host,
      range,
      timezone,
      interval: timeInterval(query),
      dimensions: query.dimensions,
      filters: query.filters,
    };
  }

  /** Every group the query produces over one range, with its metrics finished. */
  private rows(query: StatsQuery, scope: Scope): Row[] {
    const { metrics } = query;
    const pages = scope.dimensions.includes("event:page") || scope.filters.some((f) => f.dimension === "event:page");

    // Which side each metric is counted on. Visitors and visits follow the
    // question: pageviews when it is about pages, visits otherwise.
    const sessionMetrics: Metric[] = pages
      ? ["bounce_rate", "visit_duration", "views_per_visit"]
      : ["visitors", "visits", "percentage", "bounce_rate", "visit_duration", "views_per_visit", "exit_rate"];
    const eventMetrics: Metric[] = pages
      ? ["visitors", "visits", "percentage", "pageviews", "time_on_page"]
      : ["pageviews", "time_on_page"];

    const wantsSessions = metrics.some((metric) => sessionMetrics.includes(metric));
    const wantsEvents = metrics.some((metric) => eventMetrics.includes(metric));
    const wantsScroll = metrics.includes("scroll_depth");

    const events = wantsEvents ? this.eventGroups(scope, metrics.includes("time_on_page")) : new Map();
    const sessions = wantsSessions ? this.sessionGroups(scope) : new Map();
    const scroll = wantsScroll ? this.scrollGroups(scope) : new Map();

    // The visitors whose share each row is, counted the same way as the rows.
    let total: number | undefined;
    if (metrics.includes("percentage")) {
      const whole = { ...scope, dimensions: [] };
      const groups = pages ? this.eventGroups(whole, false) : this.sessionGroups(whole);
      total = groups.get(key([]))?.visitors ?? 0;
    }

    // Exits are visits; exit rate is those over the pageviews of the page left from.
    let pageviewsByPage: Map<string, Group> | undefined;
    if (metrics.includes("exit_rate")) {
      pageviewsByPage = this.eventGroups({ ...scope, dimensions: ["event:page"] }, false);
    }

    // A page report is a report of pageviews: an entry page nobody viewed in
    // the range is not a row. Anything else is every group either side saw,
    // since a visit is bucketed by when it ended and its pageviews by when
    // they happened, and those need not be the same bucket.
    const keys = new Set<string>([
      ...events.keys(),
      ...(pages ? [] : sessions.keys()),
      ...(pages && !wantsEvents ? scroll.keys() : []),
    ]);
    if (!scope.dimensions.length) keys.add(key([]));

    const keyList = [...keys];
    const rows = keyList.map((k) => {
      const dimensions = JSON.parse(k) as Value[];
      const e = events.get(k) ?? {};
      const s = sessions.get(k) ?? {};
      const side = pages ? e : s;

      return {
        dimensions: dimensions.map((value, index) => present(scope.dimensions[index], value)),
        metrics: metrics.map((metric) => {
          switch (metric) {
            case "visitors":
              return side.visitors ?? 0;
            case "visits":
              return side.visits ?? 0;
            case "pageviews":
              return e.pageviews ?? 0;
            case "views_per_visit":
              return s.visits ? round((s.pageviews ?? 0) / s.visits, 2) : 0;
            case "bounce_rate":
              return s.bounce_visits ? Math.round(((s.bounces ?? 0) * 100) / s.bounce_visits) : 0;
            case "visit_duration":
              return s.visits ? Math.round((s.duration ?? 0) / s.visits) : 0;
            case "time_on_page":
              return e.engaged_visits ? Math.round((e.engaged_time ?? 0) / 1000 / e.engaged_visits) : null;
            case "scroll_depth": {
              const depth = scroll.get(k);
              return depth?.visits ? Math.round((depth.total ?? 0) / depth.visits) : null;
            }
            case "percentage":
              return total ? round(((side.visitors ?? 0) * 100) / total, 2) : null;
            case "exit_rate": {
              const viewed = pageviewsByPage?.get(key([dimensions[0]]))?.pageviews;
              return viewed ? round(((s.visits ?? 0) * 100) / viewed, 1) : null;
            }
          }
        }),
      };
    });

    // Engagement pings are read only for time on page, and a page they alone
    // mention — its pageview fell before the range — was not viewed in it.
    if (!pages || !metrics.includes("time_on_page")) return rows;
    return rows.filter((_, index) => {
      const group = events.get(keyList[index]);
      return !group || (group.visitors ?? 0) > 0;
    });
  }

  /** Groups over pageviews and engagement, joined to their visits where a visit's attributes are asked about. */
  private eventGroups(scope: Scope, engagement: boolean): Map<string, Group> {
    const params = new Params();
    const joinVisits =
      scope.dimensions.some((dimension) => dimension.startsWith("visit:")) ||
      scope.filters.some((filter) => filter.dimension.startsWith("visit:"));

    const dimensions = scope.dimensions.map((dimension) => this.eventDimension(dimension, scope));
    const where = [
      `e.host_id = ${params.add(scope.host.id)}`,
      `e.timestamp BETWEEN ${params.add(scope.range.first)} AND ${params.add(scope.range.last)}`,
      ...(joinVisits ? VISITS_ON : []),
      ...(engagement ? [] : ["e.kind <> 1"]),
      ...scope.filters.map((filter) =>
        filterSql(filter, filter.dimension === "event:page" ? "e.pathname" : VISIT_SQL[filter.dimension], params),
      ),
    ];

    // Engagement rows are only read for time on page, and everything else has
    // to step around them when they are.
    const counted = (column: string) => (engagement ? `CASE WHEN e.kind <> 1 THEN ${column} END` : column);

    const sql = `
      SELECT ${select(dimensions)}
        COUNT(DISTINCT ${counted("e.user_id")}) AS visitors,
        COUNT(DISTINCT ${counted("e.session_id")}) AS visits,
        SUM(e.kind = 0) AS pageviews
        ${
          engagement
            ? `, SUM(CASE WHEN e.kind = 1 THEN e.engagement_time ELSE 0 END) AS engaged_time,
               COUNT(DISTINCT CASE WHEN e.kind = 1 THEN e.session_id END) AS engaged_visits`
            : ""
        }
      FROM events e
      ${joinVisits ? VISITS_JOIN : ""}
      WHERE ${where.join(" AND ")}
      ${groupBy(dimensions)}`;

    return this.groups(sql, params, dimensions.length);
  }

  /** Groups over visits, spread across time buckets where the interval is an hour or a minute. */
  private sessionGroups(scope: Scope): Map<string, Group> {
    const params = new Params();
    const smear = scope.interval === "hour" || scope.interval === "minute";

    let buckets = "";
    if (smear) {
      const spans = bucketSpans(scope.labels, scope.interval as "hour" | "minute", scope.timezone);
      if (!spans.length) return new Map();
      buckets = `WITH buckets (label, lo, hi) AS (VALUES ${spans
        .map(([label, lo, hi]) => `(${params.add(label)}, ${params.add(lo)}, ${params.add(hi)})`)
        .join(", ")})`;
    }

    const dimensions = scope.dimensions.map((dimension) => this.sessionDimension(dimension, scope));
    const pageFilters = scope.filters.filter((filter) => filter.dimension === "event:page");

    // A page filter narrows visits to those that saw the page, and the bounce
    // rate to those that landed on it. Written out in the order the statement
    // reads, since that is the order its parameters bind in.
    const bounced = () =>
      pageFilters.length ? pageFilters.map((filter) => filterSql(filter, "s.entry_page", params)).join(" AND ") : "1";

    const columns = `${select(dimensions)}
        COUNT(DISTINCT s.user_id) AS visitors,
        COUNT(*) AS visits,
        SUM(s.pageviews) AS pageviews,
        SUM(CASE WHEN ${bounced()} THEN s.is_bounce ELSE 0 END) AS bounces,
        SUM(CASE WHEN ${bounced()} THEN 1 ELSE 0 END) AS bounce_visits,
        SUM(s.duration) AS duration`;

    const where = [
      `s.host_id = ${params.add(scope.host.id)}`,
      `s.start BETWEEN ${params.add(scope.range.first - LONGEST_VISIT)} AND ${params.add(scope.range.last)}`,
      `s.timestamp >= ${params.add(scope.range.first)}`,
      ...scope.filters
        .filter((filter) => filter.dimension !== "event:page")
        .map((filter) => filterSql(filter, VISIT_SQL[filter.dimension as VisitDimension], params)),
    ];

    if (pageFilters.length) {
      where.push(`s.session_id IN (
        SELECT e.session_id FROM events e
        WHERE e.host_id = ${params.add(scope.host.id)}
          AND e.timestamp BETWEEN ${params.add(scope.range.first)} AND ${params.add(scope.range.last)}
          AND e.kind <> 1
          AND ${pageFilters.map((filter) => filterSql(filter, "e.pathname", params)).join(" AND ")}
      )`);
    }

    const sql = `
      ${buckets}
      SELECT ${columns}
      FROM sessions s
      ${smear ? "JOIN buckets b ON s.start < b.hi AND s.timestamp >= b.lo" : ""}
      WHERE ${where.join(" AND ")}
      ${groupBy(dimensions)}`;

    return this.groups(sql, params, dimensions.length);
  }

  /** Each group's average of the deepest each visit scrolled. */
  private scrollGroups(scope: Scope): Map<string, Group> {
    const params = new Params();
    const joinVisits =
      scope.dimensions.some((dimension) => dimension.startsWith("visit:")) ||
      scope.filters.some((filter) => filter.dimension.startsWith("visit:"));

    const dimensions = scope.dimensions.map((dimension) => this.eventDimension(dimension, scope));
    const where = [
      `e.host_id = ${params.add(scope.host.id)}`,
      `e.timestamp BETWEEN ${params.add(scope.range.first)} AND ${params.add(scope.range.last)}`,
      ...(joinVisits ? VISITS_ON : []),
      "e.kind = 1",
      "e.scroll_depth <= 100",
      ...scope.filters.map((filter) =>
        filterSql(filter, filter.dimension === "event:page" ? "e.pathname" : VISIT_SQL[filter.dimension], params),
      ),
    ];

    const inner = dimensions.map((expression, index) => `${expression} AS d${index}`);
    const outer = dimensions.map((_, index) => `d${index}`);

    const sql = `
      SELECT ${outer.length ? `${outer.join(", ")},` : ""} SUM(depth) AS total, COUNT(*) AS visits
      FROM (
        SELECT ${inner.length ? `${inner.join(", ")},` : ""} MAX(e.scroll_depth) AS depth
        FROM events e
        ${joinVisits ? VISITS_JOIN : ""}
        WHERE ${where.join(" AND ")}
        GROUP BY ${[...outer, "e.session_id"].join(", ")}
      )
      ${outer.length ? `GROUP BY ${outer.join(", ")}` : ""}`;

    return this.groups(sql, params, dimensions.length);
  }

  private groups(sql: string, params: Params, dimensions: number): Map<string, Group> {
    const groups = new Map<string, Group>();

    for (const row of this.db.prepare(sql).all(...params.values) as Record<string, SqliteValue>[]) {
      const values = Array.from({ length: dimensions }, (_, index) => row[`d${index}`] as Value);
      const metrics: Group = {};
      for (const [name, value] of Object.entries(row)) {
        if (!/^d\d+$/.test(name)) metrics[name] = value === null ? null : Number(value);
      }
      groups.set(key(values), metrics);
    }

    return groups;
  }

  private eventDimension(dimension: Dimension, scope: Scope): string {
    if (dimension === "event:page") return "e.pathname";
    if (dimension.startsWith("time:")) return timeBucket("e.timestamp", scope);
    return VISIT_SQL[dimension as VisitDimension];
  }

  private sessionDimension(dimension: Dimension, scope: Scope): string {
    // A page broken down by visit-level metrics is the page visits entered on.
    if (dimension === "event:page") return "s.entry_page";
    if (dimension.startsWith("time:")) {
      if (scope.interval === "hour" || scope.interval === "minute") return "b.label";
      // By when the visit was last active, within the range.
      return timeBucket(`MIN(s.timestamp, ${Math.floor(scope.range.last)})`, scope);
    }
    return VISIT_SQL[dimension as VisitDimension];
  }
}

/** Dimensions whose values are codes, and the dimension that names them. */
const LABELS: Partial<Record<Dimension, Dimension>> = {
  "visit:country": "visit:country_name",
  "visit:region": "visit:region_name",
  "visit:city": "visit:city_name",
};

/**
 * The SQL for the bucket an instant falls in, as its label.
 *
 * SQLite has no time zones, so the instant is first moved onto local wall
 * time by the zone's offset — as a `CASE` over the offsets in force across the
 * range, since a range can straddle a daylight-saving change — and then
 * formatted the way the labels are.
 */
function timeBucket(column: string, scope: Scope): string {
  const segments = offsetSegments(scope.range.first - LONGEST_VISIT, scope.range.last + LONGEST_VISIT, scope.timezone);
  const offset =
    segments.length === 1
      ? String(segments[0][1])
      : `CASE ${segments
          .slice(1)
          .map(([from], index) => `WHEN ${column} < ${from} THEN ${segments[index][1]}`)
          .join(" ")} ELSE ${segments[segments.length - 1][1]} END`;
  const local = `(${column} + ${offset})`;

  switch (scope.interval) {
    case "minute":
      return `strftime('%Y-%m-%d %H:%M:00', ${local}, 'unixepoch')`;
    case "hour":
      return `strftime('%Y-%m-%d %H:00:00', ${local}, 'unixepoch')`;
    case "day":
      return `date(${local}, 'unixepoch')`;
    case "week": {
      // Monday of the week, or the range's first day if that Monday is before it.
      const first = dateRange(scope.range, scope.timezone).first;
      return `MAX(date(${local}, 'unixepoch', '-6 days', 'weekday 1'), '${first}')`;
    }
    default:
      return `strftime('%Y-%m-01', ${local}, 'unixepoch')`;
  }
}

/** A filter as a condition on the expression it applies to. */
function filterSql(filter: Filter, expression: string, params: Params): string {
  const clauses = filter.clauses.map((clause) => clauseValue(filter.dimension, clause));
  const lower = !filter.caseSensitive;
  const subject = lower ? `LOWER(${expression})` : expression;
  const value = (clause: string) => params.add(lower ? clause.toLowerCase() : clause);

  const condition =
    filter.operator === "is" || filter.operator === "is_not"
      ? `${subject} IN (${clauses.map(value).join(", ")})`
      : `(${clauses.map((clause) => `INSTR(${subject}, ${value(clause)}) > 0`).join(" OR ")})`;

  return filter.operator === "is" || filter.operator === "contains" ? condition : `NOT ${condition}`;
}

/**
 * A filter value as it is stored. Plausible's dashboard spells "no country" as
 * two NUL bytes, its ClickHouse column's empty value, and "no city" as the
 * GeoNames id 0.
 */
function clauseValue(dimension: Filter["dimension"], clause: string): string {
  if (dimension === "visit:country" && (clause === "\u0000\u0000" || clause === "ZZ")) return "";
  if (dimension === "visit:city" && clause === "0") return "";
  return clause;
}

/** A dimension value as the response carries it. */
function present(dimension: Dimension, value: Value): Value {
  if (dimension === "visit:country_name") return value ? (COUNTRY_NAMES.of(String(value)) ?? value) : value;
  return value;
}

function select(dimensions: string[]): string {
  return dimensions.map((expression, index) => `${expression} AS d${index},`).join(" ");
}

function groupBy(dimensions: string[]): string {
  return dimensions.length ? `GROUP BY ${dimensions.map((_, index) => `d${index}`).join(", ")}` : "";
}

function key(values: Value[]): string {
  return JSON.stringify(values);
}

function round(value: number, places: number): number {
  const factor = 10 ** places;
  return Math.round(value * factor) / factor;
}

function notSet(column: string, blank: string): string {
  return `CASE WHEN ${column} = '' THEN '${blank}' ELSE ${column} END`;
}

/** What a metric reads as where there is nothing to measure, as Plausible fills it. */
function emptyValue(metric: Metric): Value {
  if (["visit_duration", "exit_rate", "scroll_depth", "time_on_page"].includes(metric)) return null;
  if (["views_per_visit", "bounce_rate", "percentage"].includes(metric)) return 0.0;
  return 0;
}

/** Plausible's order when a query names none: by time, then by the first metric, largest first. */
function defaultOrder(query: StatsQuery): [Metric | Dimension, "asc" | "desc"][] {
  const time = query.dimensions.find((dimension) => dimension.startsWith("time:"));
  const byMetric: [Metric, "desc"] = [query.metrics[0], "desc"];
  return time ? [[time, "asc"], byMetric] : [byMetric];
}

function sortRows(rows: Row[], query: StatsQuery, orderBy: [Metric | Dimension, "asc" | "desc"][]): Row[] {
  const positions = orderBy.map(([name, direction]) => {
    const metric = query.metrics.indexOf(name as Metric);
    return metric === -1
      ? { index: query.dimensions.indexOf(name as Dimension), metric: false, direction }
      : { index: metric, metric: true, direction };
  });

  return [...rows].sort((a, b) => {
    for (const { index, metric, direction } of positions) {
      const left = metric ? a.metrics[index] : a.dimensions[index];
      const right = metric ? b.metrics[index] : b.dimensions[index];
      const order = compareValues(left, right);
      if (order) return direction === "asc" ? order : -order;
    }
    return 0;
  });
}

/** Nulls sort below everything, numbers numerically, strings by code point. */
function compareValues(a: Value, b: Value): number {
  if (a === b) return 0;
  if (a === null) return -1;
  if (b === null) return 1;
  if (typeof a === "number" && typeof b === "number") return a - b;
  return String(a) < String(b) ? -1 : 1;
}

/**
 * How each metric changed, Plausible's way: bounce rate by points, exit rate
 * by points to one decimal, everything else by percent.
 */
function changes(metrics: Metric[], before: Value[], after: Value[]): Value[] {
  return metrics.map((metric, index) => {
    const old = before[index];
    const current = after[index];
    if (old === null || current === null) return null;
    const o = Number(old);
    const n = Number(current);

    if (metric === "bounce_rate") return o > 0 ? n - o : null;
    if (metric === "exit_rate") return round(n - o, 1);
    if (o === 0) return n > 0 ? 100 : 0;
    return Math.round(((n - o) / o) * 100);
  });
}

function resultIndices(labels: string[], rows: Row[]): (number | null)[] {
  const index = new Map(rows.map((row, position) => [row.dimensions[0], position]));
  return labels.map((label) => index.get(label) ?? null);
}

/** The query as it was run, in the shape Plausible echoes it back. */
function echo(
  host: QueryHost,
  query: StatsQuery,
  range: TimeRange,
  comparison: TimeRange | undefined,
  orderBy: [string, string][],
  timezone: string,
): Record<string, unknown> {
  const include: Record<string, unknown> = {
    imports: query.include.imports,
    imports_meta: query.include.importsMeta,
    time_labels: query.include.timeLabels,
    time_label_result_indices: query.include.timeLabels,
    partial_time_labels: query.include.partialTimeLabels,
    compare: query.include.compare,
    compare_match_day_of_week: query.include.compareMatchDayOfWeek,
    empty_metrics: query.include.emptyMetrics,
    present_index: query.include.presentIndex,
    trim_relative_date_range: true,
  };

  const echoed: Record<string, unknown> = {
    site_id: host.hostname,
    metrics: query.metrics,
    date_range: [isoInZone(range.first, timezone), isoInZone(range.last, timezone)],
  };
  if (comparison) {
    echoed.comparison_date_range = [isoInZone(comparison.first, timezone), isoInZone(comparison.last, timezone)];
  }
  echoed.filters = query.rawFilters;
  echoed.dimensions = query.dimensions;
  echoed.order_by = orderBy;
  echoed.include = Object.fromEntries(Object.entries(include).filter(([, value]) => value));
  if (query.pagination) echoed.pagination = query.pagination;

  return echoed;
}
