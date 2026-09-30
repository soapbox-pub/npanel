//! The query Plausible's dashboard sends to `POST /api/stats/:domain/query`,
//! and the subset of it npanel answers: every metric and dimension of the
//! dashboard's own reports, minus goals, custom properties, revenue, imports
//! and segments, which npanel doesn't record.
//!
//! Validated the way Plausible's `Dashboard.QueryParser` validates it, with
//! errors that say what was wrong, since that is what the dashboard shows.

use serde_json::Value;

use super::period::{Compare, DateRange, Interval, PERIODS};
use super::zone::{is_civil_date, CivilDate};

pub const METRICS: [&str; 10] = [
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
];

/// Dimensions that describe a visit, by what the event that began it said.
pub const VISIT_DIMENSIONS: [&str; 21] = [
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
];

pub const TIME_DIMENSIONS: [&str; 5] = ["time:minute", "time:hour", "time:day", "time:week", "time:month"];

const OPERATORS: [&str; 4] = ["is", "is_not", "contains", "contains_not"];

/// A query that can't be answered, with the sentence the dashboard shows.
#[derive(Debug)]
pub struct QueryError(pub String);

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for QueryError {}

fn err<T>(message: impl Into<String>) -> Result<T, QueryError> {
    Err(QueryError(message.into()))
}

#[derive(Clone, Debug)]
pub struct Filter {
    pub operator: String,
    pub dimension: String,
    pub clauses: Vec<String>,
    pub case_sensitive: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Include {
    pub imports: bool,
    pub imports_meta: bool,
    pub time_labels: bool,
    pub partial_time_labels: bool,
    pub compare: Option<Compare>,
    pub compare_raw: Value,
    pub compare_match_day_of_week: bool,
    pub empty_metrics: bool,
    pub present_index: bool,
}

#[derive(Clone, Debug)]
pub struct StatsQuery {
    pub date_range: DateRange,
    pub relative_date: Option<CivilDate>,
    pub metrics: Vec<String>,
    pub dimensions: Vec<String>,
    pub filters: Vec<Filter>,
    /// As sent, for echoing back.
    pub raw_filters: Vec<Value>,
    pub order_by: Option<Vec<(String, String)>>,
    pub pagination: Option<(i64, i64)>,
    pub include: Include,
}

impl StatsQuery {
    /// The interval a query's time dimension asks for, if it has one.
    pub fn interval(&self) -> Option<Interval> {
        self.dimensions.iter().find_map(|d| d.strip_prefix("time:")).and_then(Interval::parse)
    }

    /// Whether the question is about pages: a page dimension or filter.
    pub fn about_pages(&self) -> bool {
        self.dimensions.iter().any(|d| d == "event:page") || self.filters.iter().any(|f| f.dimension == "event:page")
    }
}

fn is_dimension(value: &str) -> bool {
    value == "event:page" || VISIT_DIMENSIONS.contains(&value) || TIME_DIMENSIONS.contains(&value)
}

fn show(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Parse and validate a query body.
pub fn parse_stats_query(body: &Value) -> Result<StatsQuery, QueryError> {
    let Value::Object(input) = body else {
        return err("Expected a JSON object describing the query.");
    };

    let metrics = list(input.get("metrics"), "metrics")?
        .iter()
        .map(|m| match m.as_str() {
            Some(name) if METRICS.contains(&name) => Ok(name.to_string()),
            _ => err(format!("Unknown metric '{}'.", show(m))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if metrics.is_empty() {
        return err("At least one metric is required.");
    }

    let dimensions = list(Some(input.get("dimensions").unwrap_or(&Value::Array(vec![]))), "dimensions")?
        .iter()
        .map(|d| match d.as_str() {
            Some(name) if is_dimension(name) => Ok(name.to_string()),
            _ => err(format!("Invalid dimension '{}'.", show(d))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if dimensions.iter().filter(|d| d.starts_with("time:")).count() > 1 {
        return err("Only one time dimension is allowed.");
    }

    let raw_filters = list(Some(input.get("filters").unwrap_or(&Value::Array(vec![]))), "filters")?.clone();
    let filters = raw_filters.iter().map(parse_filter).collect::<Result<Vec<_>, _>>()?;

    let query = StatsQuery {
        date_range: parse_date_range(input.get("date_range"))?,
        relative_date: parse_relative_date(input.get("relative_date"))?,
        order_by: parse_order_by(input.get("order_by"), &metrics, &dimensions)?,
        pagination: parse_pagination(input.get("pagination"))?,
        include: parse_include(input.get("include"))?,
        metrics,
        dimensions,
        filters,
        raw_filters,
    };

    validate(&query)?;
    Ok(query)
}

fn list<'a>(value: Option<&'a Value>, field: &str) -> Result<&'a Vec<Value>, QueryError> {
    match value {
        Some(Value::Array(items)) => Ok(items),
        _ => err(format!("Expected '{field}' to be a list.")),
    }
}

fn parse_date_range(value: Option<&Value>) -> Result<DateRange, QueryError> {
    match value {
        Some(Value::String(period)) if PERIODS.contains(&period.as_str()) => Ok(DateRange::Period(period.clone())),
        Some(Value::Array(pair)) if pair.len() == 2 => match (pair[0].as_str(), pair[1].as_str()) {
            (Some(first), Some(last)) if is_civil_date(first) && is_civil_date(last) => {
                if first > last {
                    return err("A custom date range has to start before it ends.");
                }
                Ok(DateRange::Custom(first.into(), last.into()))
            }
            _ => err(format!("Invalid date_range '{}'.", Value::Array(pair.clone()))),
        },
        other => err(format!("Invalid date_range '{}'.", other.map_or("undefined".into(), |v| v.to_string()))),
    }
}

fn parse_relative_date(value: Option<&Value>) -> Result<Option<CivilDate>, QueryError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(date)) if is_civil_date(date) => Ok(Some(date.clone())),
        Some(other) => err(format!("Failed to convert '{}' to date.", show(other))),
    }
}

fn parse_filter(value: &Value) -> Result<Filter, QueryError> {
    let invalid = || err(format!("Invalid filter '{value}'."));
    let Value::Array(parts) = value else { return invalid() };
    if !(3..=4).contains(&parts.len()) {
        return invalid();
    }

    let operator = parts[0].as_str().filter(|o| OPERATORS.contains(o));
    let Some(operator) = operator else {
        return err(format!("Unsupported filter operator '{}'.", show(&parts[0])));
    };
    let dimension = parts[1].as_str().filter(|d| *d == "event:page" || VISIT_DIMENSIONS.contains(d));
    let Some(dimension) = dimension else {
        return err(format!("Invalid filter dimension '{}'.", show(&parts[1])));
    };
    let Value::Array(clauses) = &parts[2] else { return invalid() };
    let clauses = clauses
        .iter()
        .map(|c| match c {
            Value::String(s) => Some(s.clone()),
            Value::Number(n) => Some(crate::util::js_number(n.as_f64().unwrap_or(0.0)).to_string()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    let Some(clauses) = clauses else { return invalid() };

    let case_sensitive = match parts.get(3) {
        Some(Value::Object(modifiers)) if modifiers.contains_key("case_sensitive") => modifiers["case_sensitive"] != Value::Bool(false),
        _ => true,
    };

    Ok(Filter { operator: operator.into(), dimension: dimension.into(), clauses, case_sensitive })
}

fn parse_order_by(value: Option<&Value>, metrics: &[String], dimensions: &[String]) -> Result<Option<Vec<(String, String)>>, QueryError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    list(Some(value), "order_by")?
        .iter()
        .map(|entry| {
            let pair = entry.as_array().filter(|p| p.len() == 2);
            let (Some(name), Some(direction)) =
                (pair.and_then(|p| p[0].as_str()), pair.and_then(|p| p[1].as_str()).filter(|d| *d == "asc" || *d == "desc"))
            else {
                return err(format!("Invalid order_by entry '{entry}'."));
            };
            if !metrics.iter().any(|m| m == name) && !dimensions.iter().any(|d| d == name) {
                return err(format!("Invalid order_by entry '{entry}'. Entry is not a queried metric or dimension."));
            }
            Ok((name.to_string(), direction.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn parse_pagination(value: Option<&Value>) -> Result<Option<(i64, i64)>, QueryError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let Value::Object(fields) = value else {
        return err("Invalid pagination.");
    };
    let integer = |v: Option<&Value>, default: i64| match v {
        None => Some(default),
        Some(v) => v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64),
    };
    let limit = integer(fields.get("limit"), 10_000).filter(|l| (1..=10_000).contains(l));
    let Some(limit) = limit else {
        return err("Pagination limit must be between 1 and 10000.");
    };
    let offset = integer(fields.get("offset"), 0).filter(|o| *o >= 0);
    let Some(offset) = offset else {
        return err("Pagination offset must be 0 or more.");
    };
    Ok(Some((limit, offset)))
}

fn parse_include(value: Option<&Value>) -> Result<Include, QueryError> {
    let empty = serde_json::Map::new();
    let include = value.and_then(Value::as_object).unwrap_or(&empty);
    let flag = |name: &str| include.get(name) == Some(&Value::Bool(true));

    let raw = include.get("compare").cloned().unwrap_or(Value::Null);
    let compare = match &raw {
        Value::Null => None,
        Value::String(s) if s == "previous_period" => Some(Compare::PreviousPeriod),
        Value::String(s) if s == "year_over_year" => Some(Compare::YearOverYear),
        Value::Array(pair) if pair.len() == 2 && pair.iter().all(|d| d.as_str().is_some_and(is_civil_date)) => {
            Some(Compare::Custom(pair[0].as_str().unwrap().into(), pair[1].as_str().unwrap().into()))
        }
        other => return err(format!("Invalid include.compare '{other}'.")),
    };

    Ok(Include {
        imports: flag("imports"),
        imports_meta: flag("imports_meta"),
        time_labels: flag("time_labels"),
        partial_time_labels: flag("partial_time_labels"),
        compare,
        compare_raw: raw,
        compare_match_day_of_week: flag("compare_match_day_of_week"),
        empty_metrics: flag("empty_metrics"),
        present_index: flag("present_index"),
    })
}

/// The combinations Plausible refuses, refused the same way.
fn validate(query: &StatsQuery) -> Result<(), QueryError> {
    let pages = query.about_pages();
    let non_time_dimension = query.dimensions.iter().any(|d| !d.starts_with("time:"));
    let has = |metric: &str| query.metrics.iter().any(|m| m == metric);

    if has("views_per_visit") && (non_time_dimension || pages) {
        return err("Metric `views_per_visit` cannot be queried with a filter on `event:page` or with a dimension other than time.");
    }
    for metric in ["time_on_page", "scroll_depth"] {
        if has(metric) && !pages {
            return err(format!("Metric `{metric}` can only be queried with event:page filters or dimensions."));
        }
    }
    if has("exit_rate") && !query.dimensions.iter().any(|d| d == "visit:exit_page") {
        return err("Metric `exit_rate` requires a `visit:exit_page` dimension.");
    }

    let period = match &query.date_range {
        DateRange::Period(p) => Some(p.as_str()),
        DateRange::Custom(..) => None,
    };
    if query.include.compare.is_some() && matches!(period, Some("realtime" | "realtime_30m" | "all")) {
        return err("Comparisons are not available for this period.");
    }
    if query.dimensions.iter().any(|d| d == "time:minute") {
        if let Some(period) = period {
            if !["realtime", "realtime_30m", "day", "24h"].contains(&period) {
                return err("The minute interval is only available for periods of a day or less.");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses() {
        let q = parse_stats_query(&json!({
            "date_range": "28d",
            "metrics": ["visitors", "bounce_rate"],
            "dimensions": ["visit:source"],
            "filters": [["is", "visit:country", ["DE"], {"case_sensitive": false}]],
            "order_by": [["visitors", "desc"]],
            "pagination": {"limit": 9, "offset": 0},
            "include": {"compare": "previous_period"}
        }))
        .unwrap();
        assert_eq!(q.pagination, Some((9, 0)));
        assert!(!q.filters[0].case_sensitive);
        assert_eq!(q.include.compare, Some(Compare::PreviousPeriod));
    }

    #[test]
    fn refuses() {
        let e = |body: Value| parse_stats_query(&body).unwrap_err().0;
        assert_eq!(e(json!({"date_range": "28d", "metrics": ["goals"]})), "Unknown metric 'goals'.");
        assert_eq!(e(json!({"date_range": "28d", "metrics": []})), "At least one metric is required.");
        assert_eq!(e(json!({"date_range": "1w", "metrics": ["visitors"]})), "Invalid date_range '\"1w\"'.");
        assert!(e(json!({"date_range": "28d", "metrics": ["time_on_page"]})).contains("event:page"));
        assert!(e(json!({"date_range": "all", "metrics": ["visitors"], "include": {"compare": "previous_period"}})).contains("Comparisons"));
        assert!(e(json!({"date_range": "7d", "metrics": ["visitors"], "dimensions": ["time:minute"]})).contains("minute interval"));
    }
}
