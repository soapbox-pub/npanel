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

/// Text made safe for an element's content or a quoted attribute value.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes() {
        assert_eq!(escape(r#"<a href="x">'&'</a>"#), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
    }

    #[test]
    fn decodes() {
        assert_eq!(decode_attribute("a &amp; b"), "a & b");
        assert_eq!(decode_attribute("&#39;self&#x27; &quot;x&quot;"), "'self' \"x\"");
        assert_eq!(decode_attribute("plain"), "plain");
    }
}
