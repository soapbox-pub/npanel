//! Redirects: a hostname whose every request is sent on to another URL, as a
//! proxy's `redir` would. For a domain that only points at another — an old
//! name, a typo, a `docs.` that moved — so that it needs no proxy in front of
//! npanel to answer it.
//!
//! Which hostnames redirect is reconciled like tunnels are: every change to
//! the table reads the enabled redirects afresh into a map swapped in whole,
//! and a request looks its hostname up there without waiting on the database.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use arc_swap::ArcSwap;
use axum::body::Body;
use http::{header, HeaderValue, Request, Response, StatusCode};
use rusqlite::Connection;

use crate::db::{Db, Redirect, REDIRECT_COLUMNS};
use crate::hostname::normalize_hostname;

/// Longer than any URL worth typing into a form, and well inside what every
/// client accepts in a `Location`.
const MAX_TARGET_LEN: usize = 2000;

struct Route {
    target: String,
    keep_path: bool,
    permanent: bool,
}

pub struct Redirects {
    db: Db,
    routes: ArcSwap<HashMap<String, Route>>,
}

impl Redirects {
    pub fn new(db: Db) -> Result<Redirects> {
        let routes = db.with(|conn| Ok(load_routes(conn)?))?;
        Ok(Redirects { db, routes: ArcSwap::from_pointee(routes) })
    }

    /// Read the enabled redirects afresh.
    pub async fn reconcile(&self) -> Result<()> {
        let routes = self.db.call(|conn| Ok(load_routes(conn)?)).await?;
        self.routes.store(Arc::new(routes));
        Ok(())
    }

    /// The redirect for a request to `hostname`, if it is one. Every method
    /// and every path is redirected: the name serves nothing of its own.
    pub fn respond<B>(&self, hostname: &str, request: &Request<B>) -> Option<Response<Body>> {
        let routes = self.routes.load();
        let route = routes.get(hostname)?;
        let path_and_query = request.uri().path_and_query().map_or("/", |pq| pq.as_str());
        // The target was checked when it was saved and the path is a valid
        // URI's, so this only fails on a header value hyper wouldn't send.
        let location = HeaderValue::try_from(location(&route.target, route.keep_path, path_and_query)).ok()?;
        let mut response = Response::builder().header(header::LOCATION, location);
        response = if route.permanent {
            // A 301 with no freshness of its own is cached until the browser
            // restarts, which would outlive an admin changing the target.
            response.status(StatusCode::MOVED_PERMANENTLY).header(header::CACHE_CONTROL, "public, max-age=3600")
        } else {
            response.status(StatusCode::FOUND).header(header::CACHE_CONTROL, "no-cache")
        };
        Some(response.body(Body::empty()).expect("response"))
    }
}

fn load_routes(conn: &Connection) -> rusqlite::Result<HashMap<String, Route>> {
    conn.prepare_cached(&format!("SELECT {REDIRECT_COLUMNS} FROM redirects WHERE enabled = 1"))?
        .query_map([], Redirect::from_row)?
        .map(|row| row.map(|r| (r.hostname, Route { target: r.target, keep_path: r.keep_path, permanent: r.permanent })))
        .collect()
}

/// Where a request is sent: the target as it is, or with the request's path
/// and query after the target's own path.
fn location(target: &str, keep_path: bool, path_and_query: &str) -> String {
    if !keep_path {
        return target.to_string();
    }
    // The request's path always starts with a slash, and the target's ends
    // with one when it has no path of its own (`https://example.com/`).
    format!("{}{path_and_query}", target.trim_end_matches('/'))
}

/// Check a target staff typed, for a redirect from `hostname`: an absolute
/// `http` or `https` URL that doesn't lead back to `hostname`. Returned as
/// `url` spells it, which is what `Location` is sent as.
pub fn parse_target(input: &str, hostname: &str, keep_path: bool) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter the address to redirect to.".into());
    }
    let url = url::Url::parse(input).map_err(|_| format!("Not a valid address: {input}. Write it out in full, like https://example.com."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Redirect to an http:// or https:// address.".into());
    }
    let Some(host) = url.host_str() else {
        return Err("The address needs a domain, like https://example.com.".into());
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Leave the username and password out of the address.".into());
    }
    if url.fragment().is_some() {
        return Err("Leave the #fragment off the address.".into());
    }
    if keep_path && url.query().is_some() {
        return Err("An address that keeps the visitor's path can't have a ?query of its own. Take it off, or turn off keeping the path.".into());
    }
    if normalize_hostname(host) == hostname {
        return Err(format!("{hostname} can't redirect to itself: every visit would be sent round again."));
    }
    let target = url.to_string();
    if target.len() > MAX_TARGET_LEN {
        return Err("That address is too long.".into());
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_path_after_the_targets() {
        assert_eq!(location("https://example.com/", true, "/a/b?c=d"), "https://example.com/a/b?c=d");
        assert_eq!(location("https://example.com/blog/", true, "/"), "https://example.com/blog/");
        assert_eq!(location("https://example.com/blog", true, "/x"), "https://example.com/blog/x");
        assert_eq!(location("https://example.com/blog?a=1", false, "/x?y"), "https://example.com/blog?a=1");
    }

    #[test]
    fn checks_targets() {
        assert_eq!(parse_target(" https://About.Example.com ", "docs.example.com", true).unwrap(), "https://about.example.com/");
        assert_eq!(parse_target("http://example.com/x?y=1", "a.example", false).unwrap(), "http://example.com/x?y=1");
        assert_eq!(parse_target("https://bücher.example/", "a.example", true).unwrap(), "https://xn--bcher-kva.example/");
        for bad in ["", "example.com", "ftp://example.com", "https://u:p@example.com", "https://example.com/#x", "https://example.com/?q"] {
            assert!(parse_target(bad, "a.example", true).is_err(), "{bad}");
        }
        assert!(parse_target("https://A.example./elsewhere", "a.example", false).is_err());
    }
}
