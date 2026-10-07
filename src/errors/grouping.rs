//! Which issue a report belongs to, and what that issue is called.
//!
//! Reports are grouped by a fingerprint, as Sentry groups them, from the
//! first of these that applies:
//!
//! 1. The fingerprint the SDK sent, with `{{ default }}` standing for the
//!    one npanel would have made.
//! 2. For each exception in the chain, its type and the site's own frames
//!    that its source map placed — the original file, and the function or
//!    failing that the line of code. A minified bundle's names and positions
//!    change with every deploy; the original's mostly don't, so the same bug
//!    stays one issue across deploys.
//! 3. An exception with none of those is its type and its message, with the
//!    parts that vary between occurrences — numbers, ids, keys — taken out.
//! 4. A report with no exception is its message, the same way.

use std::sync::OnceLock;

use regex::Regex;
use sha2::{Digest, Sha256};

use super::envelope::{Exception, Frame, Report};

/// The issue a report goes to, and how it is described.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grouping {
    pub fingerprint: String,
    pub title: String,
    pub culprit: String,
}

const MAX_TITLE: usize = 200;

pub fn group(report: &Report) -> Grouping {
    let default = default_components(report);
    let custom = !report.fingerprint.is_empty() && report.fingerprint.iter().any(|part| part != "{{ default }}");
    let components = if custom {
        report
            .fingerprint
            .iter()
            .flat_map(|part| if part == "{{ default }}" { default.clone() } else { vec![format!("custom:{part}")] })
            .collect()
    } else {
        default
    };

    let mut hasher = Sha256::new();
    for component in &components {
        hasher.update(component.as_bytes());
        hasher.update([0x1f]);
    }
    Grouping { fingerprint: hex::encode(&hasher.finalize()[..16]), title: title(report), culprit: culprit(report) }
}

fn default_components(report: &Report) -> Vec<String> {
    if !report.exceptions.is_empty() {
        return report.exceptions.iter().flat_map(exception_components).collect();
    }
    match &report.message {
        Some(message) => vec![format!("message:{}", normalize(message))],
        None => vec!["empty".into()],
    }
}

fn exception_components(exception: &Exception) -> Vec<String> {
    let mut components = vec![format!("type:{}", exception.kind)];
    let frames: Vec<String> = exception.frames.iter().filter(|f| f.in_app).filter_map(frame_component).collect();
    if frames.is_empty() {
        components.push(format!("value:{}", normalize(&exception.value)));
    } else {
        components.extend(frames);
    }
    components
}

/// A frame as grouping sees it, if its source map placed it.
fn frame_component(frame: &Frame) -> Option<String> {
    let mapped = frame.mapped.as_ref()?;
    let what = match (&mapped.function, &frame.context) {
        (Some(function), _) => function.clone(),
        (None, Some(context)) => context.line.split_whitespace().collect::<Vec<_>>().join(" "),
        (None, None) => String::new(),
    };
    Some(format!("frame:{}:{what}", mapped.source))
}

/// A message with what varies between occurrences of the same error taken
/// out.
pub fn normalize(message: &str) -> String {
    static VARYING: OnceLock<Regex> = OnceLock::new();
    let varying = VARYING.get_or_init(|| {
        Regex::new(
            r"(?ix)
            \b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b
            | \b(?:npub|nsec|note|nevent|nprofile|naddr|nrelay|ncryptsec)1[02-9ac-hj-np-z]+
            | \b(?:0x)?[0-9a-f]*[0-9][0-9a-f]*[a-f][0-9a-f]*\b
            | \b(?:0x)?[0-9a-f]*[a-f][0-9a-f]*[0-9][0-9a-f]*\b
            | \d+(?:\.\d+)?",
        )
        .expect("regex")
    });
    varying.replace_all(message.trim(), "<*>").into_owned()
}

/// `Type: value`, or the message: one line, cut short.
fn title(report: &Report) -> String {
    let full = match (report.exceptions.last(), &report.message) {
        (Some(e), _) if e.value.is_empty() => e.kind.clone(),
        (Some(e), _) => format!("{}: {}", e.kind, e.value),
        (None, Some(message)) => message.clone(),
        (None, None) => "(no message)".into(),
    };
    let first = full.lines().next().unwrap_or_default();
    match first.char_indices().nth(MAX_TITLE) {
        Some((at, _)) => format!("{}…", &first[..at]),
        None => first.to_string(),
    }
}

/// Where it happened: the innermost of the site's own frames, as the source
/// map tells it if it can — or the page, failing any.
fn culprit(report: &Report) -> String {
    let frame = report.exceptions.last().and_then(|e| e.frames.iter().rev().find(|f| f.in_app));
    match frame {
        Some(frame) => match &frame.mapped {
            Some(mapped) => named(mapped.function.as_deref(), &mapped.source),
            None => named(frame.function.as_deref(), &path_of(&frame.abs_path)),
        },
        None => report.url.as_deref().map(path_of).unwrap_or_default(),
    }
}

fn named(function: Option<&str>, file: &str) -> String {
    match function {
        Some(function) => format!("{function} ({file})"),
        None => file.to_string(),
    }
}

fn path_of(url: &str) -> String {
    url::Url::parse(url).map(|u| u.path().to_string()).unwrap_or_else(|_| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::envelope::{Context, Mapped};

    fn frame(path: &str, function: &str, mapped: Option<(&str, &str)>) -> Frame {
        Frame {
            abs_path: path.into(),
            function: Some(function.into()),
            lineno: Some(1),
            colno: Some(2),
            in_app: true,
            mapped: mapped.map(|(source, function)| Mapped { source: source.into(), function: Some(function.into()), lineno: 10, colno: 3 }),
            context: None,
        }
    }

    fn report(value: &str, frames: Vec<Frame>) -> Report {
        Report { exceptions: vec![Exception { kind: "TypeError".into(), value: value.into(), frames, ..Exception::default() }], ..Report::default() }
    }

    #[test]
    fn normalizes_messages() {
        assert_eq!(normalize("Item 42 of 7 missing"), "Item <*> of <*> missing");
        assert_eq!(normalize("event 3f9a7c00e1 not found"), "event <*> not found");
        assert_eq!(normalize("no profile for npub180cvv07tjdrrgpa0j7j7tmnyl2yr6yr7l8j4s3evf6u64th6gkwsyjh6w6"), "no profile for <*>");
        assert_eq!(normalize("id 123e4567-e89b-12d3-a456-426614174000"), "id <*>");
        // Words that happen to be hex aren't ids.
        assert_eq!(normalize("Cannot read properties of undefined (reading 'face')"), "Cannot read properties of undefined (reading 'face')");
    }

    #[test]
    fn groups_by_mapped_frames_across_deploys() {
        let a = report("x is 1", vec![frame("https://s/assets/index-AAA.js", "n", Some(("src/App.tsx", "render")))]);
        let b = report("x is 2", vec![frame("https://s/assets/index-BBB.js", "q", Some(("src/App.tsx", "render")))]);
        let c = report("x is 1", vec![frame("https://s/assets/index-AAA.js", "n", Some(("src/Other.tsx", "render")))]);
        assert_eq!(group(&a).fingerprint, group(&b).fingerprint);
        assert_ne!(group(&a).fingerprint, group(&c).fingerprint);
        assert_eq!(group(&a).title, "TypeError: x is 1");
        assert_eq!(group(&a).culprit, "render (src/App.tsx)");

        // Without a context line or a name, the file alone.
        let mut d = a.clone();
        d.exceptions[0].frames[0].mapped.as_mut().unwrap().function = None;
        d.exceptions[0].frames[0].context = Some(Context { pre: vec![], line: "  foo( bar )".into(), post: vec![] });
        assert_eq!(frame_component(&d.exceptions[0].frames[0]).unwrap(), "frame:src/App.tsx:foo( bar )");
    }

    #[test]
    fn groups_unmapped_reports_by_message() {
        let a = report("Cannot read 5", vec![frame("https://s/assets/index-AAA.js", "n", None)]);
        let b = report("Cannot read 9", vec![frame("https://s/assets/index-BBB.js", "q", None)]);
        let c = report("Something else", vec![frame("https://s/assets/index-AAA.js", "n", None)]);
        assert_eq!(group(&a).fingerprint, group(&b).fingerprint);
        assert_ne!(group(&a).fingerprint, group(&c).fingerprint);
        assert_eq!(group(&a).culprit, "n (/assets/index-AAA.js)");

        let message = |m: &str| Report { message: Some(m.into()), url: Some("https://s/page?x".into()), ..Report::default() };
        assert_eq!(group(&message("took 5ms")).fingerprint, group(&message("took 10ms")).fingerprint);
        assert_eq!(group(&message("line one\nline two")).title, "line one");
        assert_eq!(group(&message("x")).culprit, "/page");
    }

    #[test]
    fn honors_sdk_fingerprints() {
        let mut a = report("a", vec![]);
        let mut b = report("b", vec![]);
        a.fingerprint = vec!["checkout".into()];
        b.fingerprint = vec!["checkout".into()];
        assert_eq!(group(&a).fingerprint, group(&b).fingerprint);

        a.fingerprint = vec!["{{ default }}".into(), "x".into()];
        b.fingerprint = vec!["{{ default }}".into(), "x".into()];
        assert_ne!(group(&a).fingerprint, group(&b).fingerprint);

        // `{{ default }}` alone is the default.
        let plain = report("a", vec![]);
        a.fingerprint = vec!["{{ default }}".into()];
        assert_eq!(group(&a).fingerprint, group(&plain).fingerprint);
    }
}
