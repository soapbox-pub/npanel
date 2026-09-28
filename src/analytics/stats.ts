import type { Logger } from "../log.ts";
import { errFields } from "../log.ts";
import type { WorkerResponse } from "./protocol.ts";
import type { QueryEngine, QueryHost } from "./query/engine.ts";
import { type Dimension, QueryError, type StatsQuery, parseStatsQuery } from "./query/parse.ts";
import { type DateRangeInput, PERIODS } from "./query/period.ts";

/** The suggestions endpoint's filter names, and the dimension each one means. */
const SUGGESTION_DIMENSIONS: Record<string, Dimension> = {
  page: "event:page",
  entry_page: "visit:entry_page",
  exit_page: "visit:exit_page",
  source: "visit:source",
  channel: "visit:channel",
  referrer: "visit:referrer",
  utm_medium: "visit:utm_medium",
  utm_source: "visit:utm_source",
  utm_campaign: "visit:utm_campaign",
  utm_content: "visit:utm_content",
  utm_term: "visit:utm_term",
  country: "visit:country",
  region: "visit:region",
  city: "visit:city",
  screen: "visit:device",
  browser: "visit:browser",
  browser_version: "visit:browser_version",
  os: "visit:os",
  os_version: "visit:os_version",
};

/**
 * The dashboard's three questions — a query, the live visitor count, and
 * filter suggestions — answered as the HTTP responses Plausible's endpoints
 * give, for the main thread to pass on untouched.
 */
export class Stats {
  constructor(
    private readonly engine: QueryEngine,
    private readonly log: Logger,
  ) {}

  query(host: QueryHost, body: unknown, timezone: string): WorkerResponse {
    return this.answer(() => this.engine.query(host, parseStatsQuery(body), timezone));
  }

  currentVisitors(host: QueryHost): WorkerResponse {
    return this.answer(() => this.engine.currentVisitors(host));
  }

  /**
   * Suggestions take the dashboard's older query-string form: `period`,
   * `date`, `from` and `to` for the range, `filters` as a JSON list of
   * filters, and `q` for what has been typed.
   */
  suggestions(host: QueryHost, filter: string, params: Record<string, string>, timezone: string): WorkerResponse {
    return this.answer(() => {
      const dimension = SUGGESTION_DIMENSIONS[filter];
      if (!dimension) throw new QueryError(`There are no suggestions for '${filter}'.`);

      const query = parseStatsQuery({
        date_range: legacyDateRange(params),
        relative_date: params.date || null,
        metrics: ["visitors"],
        filters: params.filters ? JSON.parse(params.filters) : [],
      });

      return this.engine.suggestions(host, dimension, query as StatsQuery, params.q ?? "", timezone);
    });
  }

  private answer(run: () => unknown): WorkerResponse {
    const json = { "content-type": "application/json" };

    try {
      return { status: 200, headers: json, body: JSON.stringify(run()) };
    } catch (error) {
      if (error instanceof QueryError || error instanceof SyntaxError) {
        return { status: 400, headers: json, body: JSON.stringify({ error: error.message }) };
      }
      this.log.error("analytics_query_failed", errFields(error));
      return {
        status: 500,
        headers: json,
        body: JSON.stringify({ error: "The analytics query failed. The server log has the details." }),
      };
    }
  }
}

function legacyDateRange(params: Record<string, string>): DateRangeInput {
  if (params.period === "custom") return [params.from, params.to];
  return (PERIODS as readonly string[]).includes(params.period) ? (params.period as DateRangeInput) : "28d";
}
