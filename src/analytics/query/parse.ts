import { type Compare, type DateRangeInput, type Interval, PERIODS, type Period } from "./period.ts";
import { type CivilDate, isCivilDate } from "./zone.ts";

/**
 * The query Plausible's dashboard sends to `POST /api/stats/:domain/query`,
 * and the subset of it npanel answers: every metric and dimension of the
 * dashboard's own reports, minus goals, custom properties, revenue, imports
 * and segments, which npanel doesn't record.
 *
 * Validated the way Plausible's `Dashboard.QueryParser` validates it, with
 * errors that say what was wrong, since that is what the dashboard shows.
 */

export const METRICS = [
  "visitors",
  "visits",
  "pageviews",
  "views_per_visit",
  "bounce_rate",
  "visit_duration",
  "time_on_page",
  "scroll_depth",
  "percentage",
  "exit_rate",
] as const;
export type Metric = (typeof METRICS)[number];

/** Dimensions that describe a visit, by what the event that began it said. */
export const VISIT_DIMENSIONS = [
  "visit:source",
  "visit:referrer",
  "visit:channel",
  "visit:utm_medium",
  "visit:utm_source",
  "visit:utm_campaign",
  "visit:utm_content",
  "visit:utm_term",
  "visit:country",
  "visit:country_name",
  "visit:region",
  "visit:region_name",
  "visit:city",
  "visit:city_name",
  "visit:device",
  "visit:browser",
  "visit:browser_version",
  "visit:os",
  "visit:os_version",
  "visit:entry_page",
  "visit:exit_page",
] as const;

export const TIME_DIMENSIONS = ["time:minute", "time:hour", "time:day", "time:week", "time:month"] as const;

export type VisitDimension = (typeof VISIT_DIMENSIONS)[number];
export type TimeDimension = (typeof TIME_DIMENSIONS)[number];
export type Dimension = "event:page" | VisitDimension | TimeDimension;

export type FilterOperator = "is" | "is_not" | "contains" | "contains_not";
export type FilterDimension = "event:page" | VisitDimension;

export interface Filter {
  operator: FilterOperator;
  dimension: FilterDimension;
  clauses: string[];
  caseSensitive: boolean;
}

export interface StatsQuery {
  dateRange: DateRangeInput;
  relativeDate: CivilDate | null;
  metrics: Metric[];
  dimensions: Dimension[];
  filters: Filter[];
  /** As sent, for echoing back; the filters above are what is applied. */
  rawFilters: unknown[];
  orderBy: [Metric | Dimension, "asc" | "desc"][] | null;
  pagination: { limit: number; offset: number } | null;
  include: {
    imports: boolean;
    importsMeta: boolean;
    timeLabels: boolean;
    partialTimeLabels: boolean;
    compare: Compare | null;
    compareMatchDayOfWeek: boolean;
    emptyMetrics: boolean;
    presentIndex: boolean;
  };
}

/** A query that can't be answered, with the sentence the dashboard shows for it. */
export class QueryError extends Error {}

const OPERATORS: readonly FilterOperator[] = ["is", "is_not", "contains", "contains_not"];
const FILTER_DIMENSIONS: readonly string[] = ["event:page", ...VISIT_DIMENSIONS];
const DIMENSIONS: readonly string[] = ["event:page", ...VISIT_DIMENSIONS, ...TIME_DIMENSIONS];

/** Parse and validate a query body. Throws {@link QueryError}. */
export function parseStatsQuery(body: unknown): StatsQuery {
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    throw new QueryError("Expected a JSON object describing the query.");
  }
  const input = body as Record<string, unknown>;

  const metrics = list(input.metrics, "metrics").map((metric) => {
    if (!(METRICS as readonly unknown[]).includes(metric)) throw new QueryError(`Unknown metric '${metric}'.`);
    return metric as Metric;
  });
  if (!metrics.length) throw new QueryError("At least one metric is required.");

  const dimensions = list(input.dimensions ?? [], "dimensions").map((dimension) => {
    if (!DIMENSIONS.includes(dimension as string)) throw new QueryError(`Invalid dimension '${dimension}'.`);
    return dimension as Dimension;
  });
  if (dimensions.filter((dimension) => dimension.startsWith("time:")).length > 1) {
    throw new QueryError("Only one time dimension is allowed.");
  }

  const rawFilters = list(input.filters ?? [], "filters");
  const filters = rawFilters.map(parseFilter);

  const include = parseInclude(input.include);
  const query: StatsQuery = {
    dateRange: parseDateRange(input.date_range),
    relativeDate: parseRelativeDate(input.relative_date),
    metrics,
    dimensions,
    filters,
    rawFilters,
    orderBy: parseOrderBy(input.order_by, metrics, dimensions),
    pagination: parsePagination(input.pagination),
    include,
  };

  validate(query);
  return query;
}

/** The interval a query's time dimension asks for, if it has one. */
export function timeInterval(query: { dimensions: Dimension[] }): Interval | undefined {
  const dimension = query.dimensions.find((name) => name.startsWith("time:"));
  return dimension ? (dimension.slice("time:".length) as Interval) : undefined;
}

function list(value: unknown, field: string): unknown[] {
  if (!Array.isArray(value)) throw new QueryError(`Expected '${field}' to be a list.`);
  return value;
}

function parseDateRange(value: unknown): DateRangeInput {
  if ((PERIODS as readonly unknown[]).includes(value)) return value as Period;
  if (Array.isArray(value) && value.length === 2 && isCivilDate(value[0]) && isCivilDate(value[1])) {
    if (value[0] > value[1]) throw new QueryError("A custom date range has to start before it ends.");
    return [value[0], value[1]];
  }
  throw new QueryError(`Invalid date_range '${JSON.stringify(value)}'.`);
}

function parseRelativeDate(value: unknown): CivilDate | null {
  if (value === undefined || value === null) return null;
  if (isCivilDate(value)) return value;
  throw new QueryError(`Failed to convert '${value}' to date.`);
}

function parseFilter(value: unknown): Filter {
  if (!Array.isArray(value) || value.length < 3 || value.length > 4) {
    throw new QueryError(`Invalid filter '${JSON.stringify(value)}'.`);
  }

  const [operator, dimension, clauses, modifiers] = value;
  if (!OPERATORS.includes(operator)) {
    throw new QueryError(`Unsupported filter operator '${operator}'.`);
  }
  if (!FILTER_DIMENSIONS.includes(dimension)) {
    throw new QueryError(`Invalid filter dimension '${dimension}'.`);
  }
  if (!Array.isArray(clauses) || !clauses.every((clause) => typeof clause === "string" || typeof clause === "number")) {
    throw new QueryError(`Invalid filter '${JSON.stringify(value)}'.`);
  }

  const caseSensitive =
    modifiers && typeof modifiers === "object" && "case_sensitive" in modifiers
      ? modifiers.case_sensitive !== false
      : true;

  return { operator, dimension, clauses: clauses.map(String), caseSensitive };
}

function parseOrderBy(value: unknown, metrics: Metric[], dimensions: Dimension[]): StatsQuery["orderBy"] {
  if (value === undefined || value === null) return null;

  return list(value, "order_by").map((entry) => {
    if (!Array.isArray(entry) || entry.length !== 2 || (entry[1] !== "asc" && entry[1] !== "desc")) {
      throw new QueryError(`Invalid order_by entry '${JSON.stringify(entry)}'.`);
    }
    if (!metrics.includes(entry[0]) && !dimensions.includes(entry[0])) {
      throw new QueryError(
        `Invalid order_by entry '${JSON.stringify(entry)}'. Entry is not a queried metric or dimension.`,
      );
    }
    return [entry[0], entry[1]];
  });
}

function parsePagination(value: unknown): StatsQuery["pagination"] {
  if (value === undefined || value === null) return null;
  if (typeof value !== "object") throw new QueryError("Invalid pagination.");

  const { limit = 10_000, offset = 0 } = value as { limit?: unknown; offset?: unknown };
  if (!Number.isInteger(limit) || (limit as number) < 1 || (limit as number) > 10_000) {
    throw new QueryError("Pagination limit must be between 1 and 10000.");
  }
  if (!Number.isInteger(offset) || (offset as number) < 0) throw new QueryError("Pagination offset must be 0 or more.");

  return { limit: limit as number, offset: offset as number };
}

function parseInclude(value: unknown): StatsQuery["include"] {
  const include = (value && typeof value === "object" ? value : {}) as Record<string, unknown>;

  let compare: Compare | null = null;
  if (include.compare === "previous_period" || include.compare === "year_over_year") {
    compare = include.compare;
  } else if (
    Array.isArray(include.compare) &&
    include.compare.length === 2 &&
    isCivilDate(include.compare[0]) &&
    isCivilDate(include.compare[1])
  ) {
    compare = [include.compare[0], include.compare[1]];
  } else if (include.compare !== undefined && include.compare !== null) {
    throw new QueryError(`Invalid include.compare '${JSON.stringify(include.compare)}'.`);
  }

  return {
    imports: include.imports === true,
    importsMeta: include.imports_meta === true,
    timeLabels: include.time_labels === true,
    partialTimeLabels: include.partial_time_labels === true,
    compare,
    compareMatchDayOfWeek: include.compare_match_day_of_week === true,
    emptyMetrics: include.empty_metrics === true,
    presentIndex: include.present_index === true,
  };
}

/** The combinations Plausible refuses, refused the same way. */
function validate(query: StatsQuery): void {
  const pageContext =
    query.dimensions.includes("event:page") || query.filters.some((filter) => filter.dimension === "event:page");
  const nonTimeDimension = query.dimensions.some((dimension) => !dimension.startsWith("time:"));

  if (query.metrics.includes("views_per_visit") && (nonTimeDimension || pageContext)) {
    throw new QueryError(
      "Metric `views_per_visit` cannot be queried with a filter on `event:page` or with a dimension other than time.",
    );
  }

  for (const metric of ["time_on_page", "scroll_depth"] as const) {
    if (query.metrics.includes(metric) && !pageContext) {
      throw new QueryError(`Metric \`${metric}\` can only be queried with event:page filters or dimensions.`);
    }
  }

  if (query.metrics.includes("exit_rate") && !query.dimensions.includes("visit:exit_page")) {
    throw new QueryError("Metric `exit_rate` requires a `visit:exit_page` dimension.");
  }

  if (
    query.include.compare &&
    (query.dateRange === "realtime" || query.dateRange === "realtime_30m" || query.dateRange === "all")
  ) {
    throw new QueryError("Comparisons are not available for this period.");
  }

  if (query.dimensions.includes("time:minute") && typeof query.dateRange === "string") {
    if (!["realtime", "realtime_30m", "day", "24h"].includes(query.dateRange)) {
      throw new QueryError("The minute interval is only available for periods of a day or less.");
    }
  }
}
