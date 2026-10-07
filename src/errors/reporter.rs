//! The reporter a site with error reporting on serves, and where it reports.
//!
//! Both on the site's own origin, for the reasons the analytics tracker is:
//! a script from anywhere else would need every site's Content Security
//! Policy widened, and a visitor on the site's onion address could reach no
//! other origin. The reporter posts to a neutral path, since `sentry` and
//! `envelope` are what blocklists look for; the path Sentry's own SDKs derive
//! from a DSN is answered too, so a site that already uses one can point it
//! here instead.

use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::inject::EXPAT_MAGNET;

pub const REPORTER_PATH: &str = "/_npanel/errors.js";
pub const REPORT_PATH: &str = "/_npanel/api/report";

/// The reporter, written for npanel: see the file for what it catches.
const CODE: &str = include_str!("../../assets/errors/reporter.js");

/// Whether a request path is one reports are received at: the reporter's
/// own, or `/_npanel/api/<project>/envelope/`, which is where a Sentry SDK
/// sends what it catches given the DSN `https://<key>@<site>/_npanel/<project>`.
/// The site is the hostname's; the project and key say nothing npanel needs.
pub fn is_report_path(path: &str) -> bool {
    if path == REPORT_PATH {
        return true;
    }
    let Some(rest) = path.strip_prefix("/_npanel/api/") else { return false };
    let project = rest.strip_suffix("/envelope/").or_else(|| rest.strip_suffix("/envelope"));
    project.is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Hex sha256 of the reporter, so a new release rebuilds every reporting site.
pub fn reporter_sha256() -> &'static str {
    static SHA: OnceLock<String> = OnceLock::new();
    SHA.get_or_init(|| hex::encode(Sha256::digest(CODE.as_bytes())))
}

/// The script one build serves at [`REPORTER_PATH`]: the reporter, and the
/// call that starts it.
///
/// `release` is the version the build serves, so every report names the
/// files its stack trace is about — which is what lets a frame be mapped
/// through the source map deployed beside them, whatever has been deployed
/// since.
///
/// Wrapped in a GNU LibreJS license block, as everything npanel adds to a
/// page is, and offered under the same terms.
pub fn reporter_script(release: &str) -> String {
    let config = serde_json::json!({ "endpoint": REPORT_PATH, "release": release });
    // Both tags must start their own line, and nothing but whitespace may
    // follow the closing one, or LibreJS treats what's outside as unlicensed.
    [
        format!("// @license {EXPAT_MAGNET} Expat"),
        format!("{}({config});", CODE.trim_end()),
        "// @license-end".to_string(),
        String::new(),
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn report_paths() {
        assert!(is_report_path("/_npanel/api/report"));
        assert!(is_report_path("/_npanel/api/0/envelope/"));
        assert!(is_report_path("/_npanel/api/42/envelope"));
        assert!(!is_report_path("/_npanel/api/event"));
        assert!(!is_report_path("/_npanel/api//envelope/"));
        assert!(!is_report_path("/_npanel/api/x/envelope/"));
        assert!(!is_report_path("/api/0/envelope/"));
    }

    #[test]
    fn wraps_the_reporter() {
        let script = reporter_script("abc");
        assert!(script.starts_with("// @license magnet:"));
        assert!(script.ends_with("({\"endpoint\":\"/_npanel/api/report\",\"release\":\"abc\"});\n// @license-end\n"));
        assert_eq!(reporter_sha256().len(), 64);
    }

    /// The reporter run in a page made of stubs: `listeners` holds what it
    /// listens to, and `sent` every request it makes, as `[url, body]`.
    const PAGE: &str = r#"
        var listeners = {};
        var sent = [];
        var window = {
            addEventListener: function (name, f) { listeners[name] = f; },
            location: { href: "https://docs.example.com/a?b=1", pathname: "/a", search: "?b=1", hash: "" },
            fetch: function (url, init) { sent.push([url, init && init.body]); return Promise.resolve({ status: 404 }); },
            console: { error: function () {}, warn: function () {} },
            history: { pushState: function () {}, replaceState: function () {} },
        };
    "#;

    /// Run `then` in a page with the reporter loaded, and return what it sent.
    fn run(then: &str) -> Vec<Value> {
        let runtime = rquickjs::Runtime::new().unwrap();
        let context = rquickjs::Context::full(&runtime).unwrap();
        context.with(|ctx| {
            ctx.eval::<(), _>(PAGE).unwrap();
            ctx.eval::<(), _>(reporter_script("e1")).unwrap();
            ctx.eval::<(), _>(then).unwrap();
        });
        while runtime.is_job_pending() {
            runtime.execute_pending_job().unwrap();
        }
        let sent: String = context.with(|ctx| ctx.eval::<String, _>("JSON.stringify(sent)").unwrap());
        let sent: Vec<(String, Option<String>)> = serde_json::from_str(&sent).unwrap();
        sent.into_iter()
            .filter(|(url, _)| url == REPORT_PATH)
            .map(|(_, body)| {
                let body = body.unwrap();
                let lines: Vec<&str> = body.splitn(3, '\n').collect();
                assert_eq!(serde_json::from_str::<Value>(lines[1]).unwrap()["type"], "event");
                serde_json::from_str(lines[2]).unwrap()
            })
            .collect()
    }

    fn frames(event: &Value, exception: usize) -> Vec<(String, String, u64, u64)> {
        event["exception"]["values"][exception]["stacktrace"]["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["function"].as_str().unwrap_or("").to_string(), f["abs_path"].as_str().unwrap().to_string(), f["lineno"].as_u64().unwrap(), f["colno"].as_u64().unwrap()))
            .collect()
    }

    #[test]
    fn reads_v8_stacks() {
        let sent = run(r#"
            var e = new TypeError("x is undefined");
            e.stack = "TypeError: x is undefined\n" +
                "    at render (https://docs.example.com/assets/index-B3x.js:1:2345)\n" +
                "    at async Object.load [as run] (https://docs.example.com/assets/index-B3x.js:1:999)\n" +
                "    at https://docs.example.com/assets/vendor.js:12:7\n" +
                "    at eval (eval at boot (https://docs.example.com/boot.js:3:4), <anonymous>:1:1)\n" +
                "    at report (https://docs.example.com/_npanel/errors.js:1:1)\n" +
                "    at Array.forEach (<anonymous>)";
            listeners.error({ error: e, message: "Uncaught TypeError: x is undefined" });
        "#);
        assert_eq!(sent.len(), 1);
        let event = &sent[0];
        assert_eq!(event["release"], "e1");
        assert_eq!(event["request"]["url"], "https://docs.example.com/a?b=1");
        assert_eq!(event["exception"]["values"][0]["type"], "TypeError");
        assert_eq!(event["exception"]["values"][0]["value"], "x is undefined");
        assert_eq!(event["exception"]["values"][0]["mechanism"]["type"], "onerror");
        assert_eq!(event["event_id"].as_str().unwrap().len(), 32);
        assert_eq!(
            frames(event, 0),
            [
                // Code `eval` ran, placed where `eval` was called, as Sentry places it.
                ("eval".into(), "https://docs.example.com/boot.js".into(), 3, 4),
                ("".into(), "https://docs.example.com/assets/vendor.js".into(), 12, 7),
                ("Object.load".into(), "https://docs.example.com/assets/index-B3x.js".into(), 1, 999),
                ("render".into(), "https://docs.example.com/assets/index-B3x.js".into(), 1, 2345),
            ]
        );
    }

    #[test]
    fn reads_gecko_and_webkit_stacks() {
        let sent = run(r#"
            var e = new Error("boom");
            e.stack = "render@https://docs.example.com/assets/app.js:10:20\n" +
                "load/<@https://docs.example.com/assets/app.js:5:6\n" +
                "@https://docs.example.com/assets/app.js:1:1\n" +
                "forEach@[native code]\n";
            listeners.error({ error: e, message: "boom" });
            var s = new Error("bang");
            s.stack = "global code@https://docs.example.com/x.js:7:8";
            listeners.unhandledrejection({ reason: s });
        "#);
        assert_eq!(sent.len(), 2);
        assert_eq!(
            frames(&sent[0], 0),
            [
                ("".into(), "https://docs.example.com/assets/app.js".into(), 1, 1),
                ("load/<".into(), "https://docs.example.com/assets/app.js".into(), 5, 6),
                ("render".into(), "https://docs.example.com/assets/app.js".into(), 10, 20),
            ]
        );
        assert_eq!(frames(&sent[1], 0), [("global code".into(), "https://docs.example.com/x.js".into(), 7, 8)]);
        assert_eq!(sent[1]["exception"]["values"][0]["mechanism"]["type"], "onunhandledrejection");
    }

    #[test]
    fn reports_causes_and_odd_rejections() {
        let sent = run(r#"
            var cause = new RangeError("too far");
            cause.stack = "    at inner (https://docs.example.com/a.js:1:1)";
            var e = new Error("failed", { cause: cause });
            e.cause = cause;
            e.stack = "    at outer (https://docs.example.com/a.js:2:2)";
            listeners.error({ error: e });
            listeners.unhandledrejection({ reason: { code: 7 } });
            listeners.error({ message: "Script error." });
            listeners.error({ message: "Uncaught SyntaxError: nope", filename: "https://docs.example.com/b.js", lineno: 3, colno: 9 });
            listeners.error({});
        "#);
        assert_eq!(sent.len(), 3);
        let values = &sent[0]["exception"]["values"];
        assert_eq!((values[0]["type"].as_str(), values[1]["type"].as_str()), (Some("RangeError"), Some("Error")));
        assert_eq!(sent[1]["exception"]["values"][0]["value"], "Non-Error promise rejection captured with value: {\"code\":7}");
        assert_eq!(sent[2]["exception"]["values"][0]["value"], "Uncaught SyntaxError: nope");
        assert_eq!(frames(&sent[2], 0), [("".into(), "https://docs.example.com/b.js".into(), 3, 9)]);
    }

    #[test]
    fn keeps_a_trail_and_limits_itself() {
        let sent = run(r#"
            window.console.warn("careful", { a: 1 });
            window.history.pushState({}, "", "/b");
            window.location.pathname = "/b";
            window.history.pushState({}, "", "/b");
            window.fetch("/api/thing", { method: "post" });
        "#);
        assert!(sent.is_empty());

        let sent = run(r#"
            window.console.error("first");
            window.location.pathname = "/b";
            window.history.pushState({}, "", "/b");
            window.fetch("/api/thing", { method: "post" }).then(function () {
                for (var i = 0; i < 20; i++) {
                    var e = new Error("n" + i);
                    e.stack = "    at f (https://docs.example.com/a.js:1:" + (i + 1) + ")";
                    listeners.error({ error: e });
                    listeners.error({ error: e });
                }
            });
        "#);
        // Each error once, and no more than ten from one page.
        assert_eq!(sent.len(), 10);
        let crumbs = sent[0]["breadcrumbs"]["values"].as_array().unwrap();
        let categories: Vec<&str> = crumbs.iter().map(|c| c["category"].as_str().unwrap()).collect();
        assert_eq!(categories, ["console", "navigation", "fetch"]);
        assert_eq!(crumbs[0]["message"], "first");
        assert_eq!(crumbs[0]["level"], "error");
        assert_eq!(crumbs[1]["data"], serde_json::json!({ "from": "/a?b=1", "to": "/b?b=1" }));
        assert_eq!(crumbs[2]["data"], serde_json::json!({ "method": "POST", "url": "/api/thing", "status_code": 404 }));
        assert_eq!(crumbs[2]["level"], "error");
    }
}
