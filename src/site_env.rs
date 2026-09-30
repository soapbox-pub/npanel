//! A site's environment: variables staff set on a host, which its pages read as
//! `window.ENV`.
//!
//! A static site has no server to read configuration from, so the one build
//! goes everywhere with whatever it was built with. This is the browser half of
//! the pattern Remix documents: the server — here, the gateway — renders the
//! values into a script that runs before the site's own, and code that knows to
//! look reads `window.ENV.API_URL` instead of a constant baked in at build time.
//! The same deploy can then be served on two hosts pointed at two backends.
//!
//! The script is a file of the host's build like any other, and loaded from
//! the start of every page's head, as a classic script, so it has run before
//! anything the site wrote starts. Everything in it is public: it is served to
//! every visitor, and nothing here is fit for a secret.
//!
//! The name and value rules have a TypeScript twin in
//! `dashboard/src/shared/env.ts`, so a variable a form accepts is one the
//! server will store. Change both together.

use rusqlite::Connection;

use crate::inject::EXPAT_MAGNET;

/// Where a host's variables are served.
pub const ENV_PATH: &str = "/.npanel/env.js";

/// Most variables one host may have. Every page waits on the script before it
/// renders anything, so it is kept to configuration rather than content.
pub const MAX_VARIABLES: usize = 100;

/// Longest name npanel will store.
const MAX_NAME_LENGTH: usize = 128;

/// Longest value npanel will store, in characters.
const MAX_VALUE_LENGTH: usize = 4096;

/// Accept a variable name, exactly as written apart from surrounding space.
///
/// Names follow the rule shells hold environment variables to — letters,
/// digits and underscores, not starting with a digit — which makes every one
/// a plain identifier, readable as `window.ENV.NAME` without brackets. Case is
/// kept: `api_url` and `API_URL` are two variables, as they are everywhere
/// else environment variables exist.
pub fn parse_env_name(input: &str) -> Result<String, String> {
    let name = input.trim();

    if name.is_empty() {
        return Err("Enter a name for this variable.".into());
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        return Err(format!("A name can be at most {MAX_NAME_LENGTH} characters."));
    }
    let mut chars = name.chars();
    let starts_well = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    if !starts_well || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!("A name may only use letters, digits, and _, and can't start with a digit — {name} doesn't fit."));
    }

    Ok(name.to_string())
}

/// Accept a value. Kept verbatim, spaces and all: it is the site's to
/// interpret, and an empty one is a value like any other.
pub fn parse_env_value(input: &str) -> Result<String, String> {
    if input.chars().count() > MAX_VALUE_LENGTH {
        return Err(format!("A value can be at most {MAX_VALUE_LENGTH} characters."));
    }
    Ok(input.to_string())
}

/// A host's variables, ordered by name. Blocking.
pub fn host_env(conn: &Connection, host_id: i64) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare_cached("SELECT name, value FROM host_env WHERE host_id = ? ORDER BY name")?
        .query_map([host_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}

/// The script a host serves at [`ENV_PATH`].
///
/// The values go in as JSON, which is a JavaScript literal — apart from U+2028
/// and U+2029, which engines older than ES2019 read as line breaks inside a
/// string, so they are escaped.
///
/// The whole file is wrapped in a GNU LibreJS license block, without which a
/// visitor running the extension would have the script blocked and the site
/// would run with no `window.ENV` at all. It is offered under the same terms
/// as the tracker.
pub fn env_script(vars: &[(String, String)]) -> String {
    let values: serde_json::Map<String, serde_json::Value> = vars.iter().map(|(name, value)| (name.clone(), value.clone().into())).collect();
    let literal = serde_json::Value::Object(values).to_string().replace('\u{2028}', "\\u2028").replace('\u{2029}', "\\u2029");

    // Both tags must start their own line, and nothing but whitespace may
    // follow the closing one, or LibreJS treats what's outside as unlicensed.
    [format!("// @license {EXPAT_MAGNET} Expat"), format!("window.ENV = {literal};"), "// @license-end".to_string(), String::new()].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(parse_env_name("  API_URL ").unwrap(), "API_URL");
        assert_eq!(parse_env_name("_private2").unwrap(), "_private2");
        assert_eq!(parse_env_name("camelCase").unwrap(), "camelCase");
        assert!(parse_env_name("").is_err());
        assert!(parse_env_name("2FAST").is_err());
        assert!(parse_env_name("API-URL").is_err());
        assert!(parse_env_name("A B").is_err());
        assert!(parse_env_name("ÉTÉ").is_err());
        assert!(parse_env_name(&"A".repeat(128)).is_ok());
        assert!(parse_env_name(&"A".repeat(129)).is_err());
    }

    #[test]
    fn values() {
        assert_eq!(parse_env_value(" spaced ").unwrap(), " spaced ");
        assert_eq!(parse_env_value("").unwrap(), "");
        assert!(parse_env_value(&"é".repeat(4096)).is_ok());
        assert!(parse_env_value(&"x".repeat(4097)).is_err());
    }

    #[test]
    fn scripts() {
        let script = env_script(&[("API_URL".into(), "https://api.example.com".into()), ("NOTE".into(), "</script>\u{2028}\"".into())]);
        assert_eq!(
            script,
            format!(
                "// @license {EXPAT_MAGNET} Expat\nwindow.ENV = {{\"API_URL\":\"https://api.example.com\",\"NOTE\":\"</script>\\u2028\\\"\"}};\n// @license-end\n"
            )
        );
    }
}
