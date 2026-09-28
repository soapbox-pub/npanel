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

/** One site's last 24 hours, as the list of sites draws it. */
export interface Sparkline {
  visitors: number;
  /** Percent change against the 24 hours before, as Plausible reckons it: 100 from nothing. */
  change: number | null;
  /** Visitors in each hour, oldest first; the first and last hours are partial. */
  hours: number[];
}

/** The parts of the engine's answers a sparkline is read from. */
interface TotalAnswer {
  results: { metrics: (number | null)[]; comparison?: { change: (number | null)[] | null } }[];
}

interface SeriesAnswer {
  results: { metrics: (number | null)[] }[];
  meta: { time_label_result_indices: (number | null)[] };
}

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
   * The last 24 hours of each site, the way Plausible's list of sites shows
   * them: unique visitors, their change against the 24 hours before, and the
   * visitors of each hour.
   *
   * Two queries a site, since hourly buckets don't add up to a day's unique
   * visitors. The comparison is instant for instant rather than a week back,
   * which is what "since yesterday" means on a list glanced at.
   */
  sparklines(hosts: QueryHost[], timezone: string): WorkerResponse {
    return this.answer(() => {
      const total = parseStatsQuery({
        date_range: "24h",
        metrics: ["visitors"],
        include: { compare: "previous_period", compare_match_day_of_week: false },
      });
      const series = parseStatsQuery({
        date_range: "24h",
        metrics: ["visitors"],
        dimensions: ["time:hour"],
        include: { time_labels: true },
      });

      const sparklines: Record<string, Sparkline> = {};
      for (const host of hosts) {
        const [row] = (this.engine.query(host, total, timezone) as unknown as TotalAnswer).results;
        const hourly = this.engine.query(host, series, timezone) as unknown as SeriesAnswer;

        sparklines[host.hostname] = {
          visitors: Number(row?.metrics[0] ?? 0),
          change: row?.comparison?.change?.[0] ?? null,
          hours: hourly.meta.time_label_result_indices.map((index) =>
            index === null ? 0 : Number(hourly.results[index].metrics[0] ?? 0),
          ),
        };
      }

      return { sparklines };
    });
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
