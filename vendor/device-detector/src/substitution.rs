use std::borrow::Cow;

/// Replace `$1`, `$2`, ... in `template` with capture groups from the regex
/// match, then trim trailing whitespace and dots (matching Matomo PHP behaviour).
///
/// Returns borrowed data when the template contains no `$N` placeholders,
/// avoiding allocation entirely in that case.
pub(crate) fn substitute<'a>(template: &'a str, captures: &crate::program::Caps) -> Cow<'a, str> {
    // Fast path: no placeholders → borrow directly from the template.
    if !template.contains('$') {
        return Cow::Borrowed(template.trim_end_matches(|c: char| c.is_whitespace() || c == '.'));
    }

    let mut result = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            if let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    chars.next();
                    let idx = (d as u8 - b'0') as usize;
                    if let Some(m) = captures.get(idx) {
                        result.push_str(m);
                    }
                    continue;
                }
            }
        }
        result.push(c);
    }

    let trimmed_len = result
        .trim_end_matches(|c: char| c.is_whitespace() || c == '.')
        .len();
    result.truncate(trimmed_len);
    Cow::Owned(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::Program;

    fn check(pattern: &str, text: &str, template: &str) -> String {
        let program = Program::compile(pattern).unwrap();
        substitute(template, &program.captures(text).unwrap()).into_owned()
    }

    #[test]
    fn basic_substitution() {
        assert_eq!(check(r"(Chrome)/(\d+)\.(\d+)", "Chrome/120.0", "$1 v$2.$3"), "Chrome v120.0");
    }

    #[test]
    fn no_placeholders() {
        assert_eq!(check(r"(Chrome)", "Chrome", "Safari"), "Safari");
    }

    #[test]
    fn missing_group_is_ignored() {
        assert_eq!(check(r"(Chrome)", "Chrome", "$1 $2"), "Chrome");
    }
}
