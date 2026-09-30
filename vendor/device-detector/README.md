# device-detector (vendored)

A Rust port of [Matomo's device-detector](https://github.com/matomo-org/device-detector),
vendored from `device-detector-rs` 0.1.1 (MIT, see `LICENSE`) with the changes
npanel needs:

- **The database is loaded from memory** (`DeviceDetector::from_source`), so
  npanel embeds Matomo's YAML in its binary rather than shipping a directory.
- **Clients are matched in Matomo's order** — feed readers, mobile apps, media
  players, PIM, browsers, libraries. Browsers first read every in-app browser
  (Facebook, Instagram) as the webview it embeds.
- **Versions use dots**, as Matomo's `buildVersion` does: iOS `17_5` is `17.5`.
- **Patterns are compiled on first use.** The database is twenty thousand
  patterns, mostly device models real traffic never reaches; compiling them all
  up front took half a gigabyte and two seconds. `compiled_patterns()` reports
  how many have been, so a long-lived owner can replace its detector.
- **Perl classes are ASCII**, as PCRE reads them without the `u` flag, which is
  how Matomo runs them. Unicode `\d`/`\w` made every program ~100× larger.
- **Lookaround patterns are gated** by a lookaround-free superset the `regex`
  crate can run, so `fancy_regex`'s backtracking VM only runs when it might
  match (`src/gate.rs`).
- **Heuristic patterns are compiled once**, not on every parse.
- **Device brands are prefiltered** by their literal prefixes, like the flat
  parsers already were, and a literal set with a short member is not used as a
  filter at all (it made those patterns unreachable).

Together these took a parse from ~20 ms to well under 1 ms with identical
output on the test corpus.
