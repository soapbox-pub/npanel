//! HTML text as a browser reads it.

/// Decode the character references in an attribute value, the way an HTML
/// parser does when it reads the attribute: `&amp;` is `&`, `&#39;` is `'`.
pub fn decode_attribute(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }

    // The parser that reads documents is the one that knows every named
    // reference, so it is handed the value as an attribute to read back.
    let quote = if raw.contains('"') { '\'' } else { '"' };
    let fragment = scraper::Html::parse_fragment(&format!("<i title={quote}{raw}{quote}></i>"));
    let selector = scraper::Selector::parse("i").expect("selector");
    fragment
        .select(&selector)
        .next()
        .and_then(|element| element.value().attr("title"))
        .unwrap_or(raw)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes() {
        assert_eq!(decode_attribute("a &amp; b"), "a & b");
        assert_eq!(decode_attribute("&#39;self&#x27; &quot;x&quot;"), "'self' \"x\"");
        assert_eq!(decode_attribute("plain"), "plain");
    }
}
