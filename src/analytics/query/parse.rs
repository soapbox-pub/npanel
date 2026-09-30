//! The query Plausible's dashboard sends to `POST /api/stats/:domain/query`,
//! and the subset of it npanel answers: every metric and dimension of the
//! dashboard's own reports, minus goals, custom properties, revenue, imports
//! and segments, which npanel doesn't record.
//!
//! Validated the way Plausible's `Dashboard.QueryParser` validates it, with
//! errors that say what was wrong, since that is what the dashboard shows.

use chrono::NaiveDate;
use serde_json::Value;

use super::period::{Compare, DateRange, Interval, Period};
use super::zone::parse_date;

/// What a query can measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Visitors,
    Visits,
    Pageviews,
    ViewsPerVisit,
    BounceRate,
    VisitDuration,
    TimeOnPage,
    ScrollDepth,
    Percentage,
    ExitRate,
}

impl Metric {
    const ALL: [Metric; 10] = [
        Metric::Visitors,
        Metric::Visits,
        Metric::Pageviews,
        Metric::ViewsPerVisit,
        Metric::BounceRate,
        Metric::VisitDuration,
        Metric::TimeOnPage,
        Metric::ScrollDepth,
        Metric::Percentage,
        Metric::ExitRate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Metric::Visitors => "visitors",
            Metric::Visits => "visits",
            Metric::Pageviews => "pageviews",
            Metric::ViewsPerVisit => "views_per_visit",
            Metric::BounceRate => "bounce_rate",
            Metric::VisitDuration => "visit_duration",
            Metric::TimeOnPage => "time_on_page",
            Metric::ScrollDepth => "scroll_depth",
            Metric::Percentage => "percentage",
            Metric::ExitRate => "exit_rate",
        }
    }

    fn parse(name: &str) -> Option<Metric> {
        Metric::ALL.into_iter().find(|m| m.name() == name)
    }
}

/// What describes a visit, by what the event that began it said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisitDimension {
    Source,
    Referrer,
    Channel,
    UtmMedium,
    UtmSource,
    UtmCampaign,
    UtmContent,
    UtmTerm,
    Country,
    CountryName,
    Region,
    RegionName,
    City,
    CityName,
    Device,
    Browser,
    BrowserVersion,
    Os,
    OsVersion,
    EntryPage,
    ExitPage,
}

impl VisitDimension {
    const ALL: [VisitDimension; 21] = [
        VisitDimension::Source,
        VisitDimension::Referrer,
        VisitDimension::Channel,
        VisitDimension::UtmMedium,
        VisitDimension::UtmSource,
        VisitDimension::UtmCampaign,
        VisitDimension::UtmContent,
        VisitDimension::UtmTerm,
        VisitDimension::Country,
        VisitDimension::CountryName,
        VisitDimension::Region,
        VisitDimension::RegionName,
        VisitDimension::City,
        VisitDimension::CityName,
        VisitDimension::Device,
        VisitDimension::Browser,
        VisitDimension::BrowserVersion,
        VisitDimension::Os,
        VisitDimension::OsVersion,
        VisitDimension::EntryPage,
        VisitDimension::ExitPage,
    ];

    pub fn name(self) -> &'static str {
        match self {
            VisitDimension::Source => "visit:source",
            VisitDimension::Referrer => "visit:referrer",
            VisitDimension::Channel => "visit:channel",
            VisitDimension::UtmMedium => "visit:utm_medium",
            VisitDimension::UtmSource => "visit:utm_source",
            VisitDimension::UtmCampaign => "visit:utm_campaign",
            VisitDimension::UtmContent => "visit:utm_content",
            VisitDimension::UtmTerm => "visit:utm_term",
            VisitDimension::Country => "visit:country",
            VisitDimension::CountryName => "visit:country_name",
            VisitDimension::Region => "visit:region",
            VisitDimension::RegionName => "visit:region_name",
            VisitDimension::City => "visit:city",
            VisitDimension::CityName => "visit:city_name",
            VisitDimension::Device => "visit:device",
            VisitDimension::Browser => "visit:browser",
            VisitDimension::BrowserVersion => "visit:browser_version",
            VisitDimension::Os => "visit:os",
            VisitDimension::OsVersion => "visit:os_version",
            VisitDimension::EntryPage => "visit:entry_page",
            VisitDimension::ExitPage => "visit:exit_page",
        }
    }
}

/// What a query can break its answer down by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    Page,
    Visit(VisitDimension),
    Time(Interval),
}

impl Dimension {
    pub fn name(self) -> String {
        match self {
            Dimension::Page => "event:page".into(),
            Dimension::Visit(visit) => visit.name().into(),
            Dimension::Time(interval) => format!("time:{}", interval.name()),
        }
    }

    pub fn parse(name: &str) -> Option<Dimension> {
        if name == "event:page" {
            return Some(Dimension::Page);
        }
        if let Some(interval) = name.strip_prefix("time:") {
            return Interval::ALL.into_iter().find(|i| i.name() == interval).map(Dimension::Time);
        }
        VisitDimension::ALL.into_iter().find(|v| v.name() == name).map(Dimension::Visit)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operator {
    Is,
    IsNot,
    Contains,
    ContainsNot,
}

impl Operator {
    fn parse(name: &str) -> Option<Operator> {
        Some(match name {
            "is" => Operator::Is,
            "is_not" => Operator::IsNot,
            "contains" => Operator::Contains,
            "contains_not" => Operator::ContainsNot,
            _ => return None,
        })
    }

    /// Whether the clauses are matched exactly rather than as substrings.
    pub fn exact(self) -> bool {
        matches!(self, Operator::Is | Operator::IsNot)
    }

    pub fn negated(self) -> bool {
        matches!(self, Operator::IsNot | Operator::ContainsNot)
    }
}

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

/// What a filter can be on: a page, or an attribute of a visit. Time is
/// the date range's business.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subject {
    Page,
    Visit(VisitDimension),
}

/// A condition on what a query counts.
#[derive(Clone, Debug)]
pub struct Filter {
    pub operator: Operator,
    pub subject: Subject,
    pub clauses: Vec<String>,
    pub case_sensitive: bool,
}

/// What a query's rows are ordered by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Metric(Metric),
    Dimension(Dimension),
}

impl Sort {
    pub fn name(self) -> String {
        match self {
            Sort::Metric(metric) => metric.name().into(),
            Sort::Dimension(dimension) => dimension.name(),
        }
    }
}

/// One ordering, and whether it is ascending.
pub type Order = (Sort, bool);

#[derive(Clone, Debug, Default)]
pub struct Include {
    pub imports: bool,
    pub imports_meta: bool,
    pub time_labels: bool,
    pub partial_time_labels: bool,
    pub compare: Option<Compare>,
    /// As sent, for echoing back.
    pub compare_raw: Value,
    pub compare_match_day_of_week: bool,
    pub empty_metrics: bool,
    pub present_index: bool,
}

#[derive(Clone, Debug)]
pub struct StatsQuery {
    pub date_range: DateRange,
    pub relative_date: Option<NaiveDate>,
    pub metrics: Vec<Metric>,
    pub dimensions: Vec<Dimension>,
    pub filters: Vec<Filter>,
    /// As sent, for echoing back.
    pub raw_filters: Vec<Value>,
    pub order_by: Option<Vec<Order>>,
    pub pagination: Option<(i64, i64)>,
    pub include: Include,
}

impl StatsQuery {
    /// A query over a period for some metrics, and nothing else.
    pub fn new(period: Period, metrics: Vec<Metric>) -> StatsQuery {
        StatsQuery {
            date_range: DateRange::Period(period),
            relative_date: None,
            metrics,
            dimensions: Vec::new(),
            filters: Vec::new(),
            raw_filters: Vec::new(),
            order_by: None,
            pagination: None,
            include: Include::default(),
        }
    }

    /// The interval a query's time dimension asks for, if it has one.
    pub fn interval(&self) -> Option<Interval> {
        self.dimensions.iter().find_map(|d| match d {
            Dimension::Time(interval) => Some(*interval),
            _ => None,
        })
    }

    /// Whether the question is about pages: a page dimension or filter.
    pub fn about_pages(&self) -> bool {
        self.dimensions.contains(&Dimension::Page) || self.filters.iter().any(|f| f.subject == Subject::Page)
    }

    pub fn has(&self, metric: Metric) -> bool {
        self.metrics.contains(&metric)
    }
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
        .map(|m| m.as_str().and_then(Metric::parse).ok_or_else(|| QueryError(format!("Unknown metric '{}'.", show(m)))))
        .collect::<Result<Vec<_>, _>>()?;
    if metrics.is_empty() {
        return err("At least one metric is required.");
    }

    let dimensions = list(Some(input.get("dimensions").unwrap_or(&Value::Array(vec![]))), "dimensions")?
        .iter()
        .map(|d| d.as_str().and_then(Dimension::parse).ok_or_else(|| QueryError(format!("Invalid dimension '{}'.", show(d)))))
        .collect::<Result<Vec<_>, _>>()?;
    if dimensions.iter().filter(|d| matches!(d, Dimension::Time(_))).count() > 1 {
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

/// Two dates, as a query sends a custom range.
fn date_pair(pair: &[Value]) -> Option<(NaiveDate, NaiveDate)> {
    match pair {
        [first, last] => Some((parse_date(first.as_str()?)?, parse_date(last.as_str()?)?)),
        _ => None,
    }
}

fn parse_date_range(value: Option<&Value>) -> Result<DateRange, QueryError> {
    let invalid = || err(format!("Invalid date_range '{}'.", value.map_or("undefined".into(), |v| v.to_string())));
    match value {
        Some(Value::String(period)) => Period::parse(period).map(DateRange::Period).map_or_else(invalid, Ok),
        Some(Value::Array(pair)) => match date_pair(pair) {
            Some((first, last)) if first > last => err("A custom date range has to start before it ends."),
            Some((first, last)) => Ok(DateRange::Custom(first, last)),
            None => invalid(),
        },
        _ => invalid(),
    }
}

fn parse_relative_date(value: Option<&Value>) -> Result<Option<NaiveDate>, QueryError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(other) => match other.as_str().and_then(parse_date) {
            Some(date) => Ok(Some(date)),
            None => err(format!("Failed to convert '{}' to date.", show(other))),
        },
    }
}

fn parse_filter(value: &Value) -> Result<Filter, QueryError> {
    let invalid = || err(format!("Invalid filter '{value}'."));
    let Value::Array(parts) = value else { return invalid() };
    if !(3..=4).contains(&parts.len()) {
        return invalid();
    }

    let Some(operator) = parts[0].as_str().and_then(Operator::parse) else {
        return err(format!("Unsupported filter operator '{}'.", show(&parts[0])));
    };
    let subject = match parts[1].as_str().and_then(Dimension::parse) {
        Some(Dimension::Page) => Subject::Page,
        Some(Dimension::Visit(visit)) => Subject::Visit(visit),
        _ => return err(format!("Invalid filter dimension '{}'.", show(&parts[1]))),
    };
    let Value::Array(clauses) = &parts[2] else { return invalid() };
    let clauses = clauses
        .iter()
        .map(|c| match c {
            Value::String(s) => Some(s.clone()),
            // The dashboard sends a city as its GeoNames id.
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>();
    let Some(clauses) = clauses else { return invalid() };

    let case_sensitive = match parts.get(3) {
        Some(Value::Object(modifiers)) if modifiers.contains_key("case_sensitive") => modifiers["case_sensitive"] != Value::Bool(false),
        _ => true,
    };

    Ok(Filter { operator, subject, clauses, case_sensitive })
}

fn parse_order_by(value: Option<&Value>, metrics: &[Metric], dimensions: &[Dimension]) -> Result<Option<Vec<Order>>, QueryError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    list(Some(value), "order_by")?
        .iter()
        .map(|entry| {
            let pair = entry.as_array().filter(|p| p.len() == 2);
            let name = pair.and_then(|p| p[0].as_str());
            let ascending = pair.and_then(|p| match p[1].as_str() {
                Some("asc") => Some(true),
                Some("desc") => Some(false),
                _ => None,
            });
            let (Some(name), Some(ascending)) = (name, ascending) else {
                return err(format!("Invalid order_by entry '{entry}'."));
            };
            let sort = match (Metric::parse(name), Dimension::parse(name)) {
                (Some(metric), _) if metrics.contains(&metric) => Sort::Metric(metric),
                (_, Some(dimension)) if dimensions.contains(&dimension) => Sort::Dimension(dimension),
                _ => return err(format!("Invalid order_by entry '{entry}'. Entry is not a queried metric or dimension.")),
            };
            Ok((sort, ascending))
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
        Value::Array(pair) if date_pair(pair).is_some() => date_pair(pair).map(|(first, last)| Compare::Custom(first, last)),
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
    let non_time_dimension = query.dimensions.iter().any(|d| !matches!(d, Dimension::Time(_)));

    if query.has(Metric::ViewsPerVisit) && (non_time_dimension || pages) {
        return err("Metric `views_per_visit` cannot be queried with a filter on `event:page` or with a dimension other than time.");
    }
    for metric in [Metric::TimeOnPage, Metric::ScrollDepth] {
        if query.has(metric) && !pages {
            return err(format!("Metric `{}` can only be queried with event:page filters or dimensions.", metric.name()));
        }
    }
    if query.has(Metric::ExitRate) && !query.dimensions.contains(&Dimension::Visit(VisitDimension::ExitPage)) {
        return err("Metric `exit_rate` requires a `visit:exit_page` dimension.");
    }

    let period = match query.date_range {
        DateRange::Period(period) => Some(period),
        DateRange::Custom(..) => None,
    };
    if query.include.compare.is_some() && matches!(period, Some(Period::Realtime | Period::Realtime30m | Period::All)) {
        return err("Comparisons are not available for this period.");
    }
    if query.dimensions.contains(&Dimension::Time(Interval::Minute))
        && period.is_some_and(|p| !matches!(p, Period::Realtime | Period::Realtime30m | Period::Day | Period::Hours24))
    {
        return err("The minute interval is only available for periods of a day or less.");
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
        assert_eq!(q.order_by, Some(vec![(Sort::Metric(Metric::Visitors), false)]));
        for dimension in VisitDimension::ALL.map(Dimension::Visit).into_iter().chain([Dimension::Page]).chain(Interval::ALL.map(Dimension::Time)) {
            assert_eq!(Dimension::parse(&dimension.name()), Some(dimension));
        }
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
        assert!(e(json!({"date_range": "7d", "metrics": ["visitors"], "filters": [["is", "time:day", ["x"]]]})).contains("filter dimension"));
    }
}
