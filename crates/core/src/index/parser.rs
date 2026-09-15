//! Minimal desktop entry parser.
//!
//! This parses the subset of the [freedesktop Desktop Entry
//! Specification](https://specifications.freedesktop.org/desktop-entry-spec/latest/)
//! that the indexer needs. A dedicated library was deliberately not added:
//! the workspace keeps dependencies minimal, and this module only has to
//! read six keys with well-defined syntax, all covered by unit tests.
//!
//! The parser is UTF-8 based, like GLib's `GKeyFile`, and operates on the
//! whole file at once:
//!
//! * `#` comments and blank lines are ignored,
//! * group headers (`[Name]`) delimit sections; only `[Desktop Entry]` keys
//!   are collected,
//! * keys without locality (`Name`, not `Name[de]`) are read,
//! * values are unescaped per the specification (`\s`, `\n`, `\t`, `\r`,
//!   `\\`; unknown escapes are kept verbatim),
//! * `Exec` values are kept as written (quoting and `%` placeholders are
//!   preserved for later execution),
//! * booleans accept `true`/`false` case-insensitively plus `1`/`0`,
//! * structurally invalid lines (malformed group header, content outside a
//!   group, a non-comment line without `=` inside `[Desktop Entry]`) fail the
//!   whole file, which the indexer then skips.

use std::fmt;

/// The group the indexer is interested in.
const DESKTOP_ENTRY_GROUP: &str = "Desktop Entry";

/// A single parsed desktop entry, before any filtering.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ParsedEntry {
    /// `Name`.
    pub name: Option<String>,
    /// `Exec`, formatted exactly as stored in the file.
    pub exec: Option<String>,
    /// `Icon`.
    pub icon: Option<String>,
    /// `Type`.
    pub entry_type: Option<String>,
    /// `true` when `NoDisplay` is `true` (case-insensitive / `1`).
    pub no_display: bool,
}

/// Structural problems that make a whole file invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseError {
    /// A line that should be a group header, e.g. `[...`, is malformed.
    MalformedGroup { line: usize },
    /// Content appears before any group header.
    ContentOutsideGroup { line: usize },
    /// A line inside `[Desktop Entry]` is neither a comment nor `key=value`.
    MissingEqual { line: usize },
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MalformedGroup { line } => {
                write!(f, "line {line}: malformed group header")
            }
            ParseError::ContentOutsideGroup { line } => {
                write!(f, "line {line}: content before any group header")
            }
            ParseError::MissingEqual { line } => {
                write!(f, "line {line}: expected `key=value`")
            }
        }
    }
}

impl std::error::Error for ParseError {}

/// Parse `contents` and return the `[Desktop Entry]` group.
pub(crate) fn parse(contents: &str) -> Result<ParsedEntry, ParseError> {
    let mut entry = ParsedEntry::default();
    // The current group, if any. A `&str` borrowed from `contents` is enough
    // because keys are only compared to the constant group name.
    let mut group: Option<&str> = None;

    for (index, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        let line_number = index + 1;

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') {
            // Group header: `[Name]`. Anything after the closing `]` (e.g.
            // trailing whitespace) is ignored.
            match line[1..].find(']') {
                Some(close) => group = Some(line[1..1 + close].trim()),
                None => return Err(ParseError::MalformedGroup { line: line_number }),
            }
            continue;
        }

        let Some(current_group) = group else {
            return Err(ParseError::ContentOutsideGroup { line: line_number });
        };

        // Keys from other groups are irrelevant; ignore them entirely.
        if current_group != DESKTOP_ENTRY_GROUP {
            continue;
        }

        let Some(equal) = line.find('=') else {
            return Err(ParseError::MissingEqual { line: line_number });
        };

        let key = line[..equal].trim();
        // The Desktop Entry group; collect only the keys we care about. The
        // value keeps its quoting for `Exec`; escapes are resolved below.
        let value = unescape(line[equal + 1..].trim());

        // Localized variants (e.g. `Name[de]`) are ignored; the bare key is
        // the fallback that this phase uses.
        if key.contains('[') {
            continue;
        }

        match key {
            "Name" => entry.name = Some(value),
            "Exec" => entry.exec = Some(value),
            "Icon" => entry.icon = Some(value),
            "Type" => entry.entry_type = Some(value),
            "NoDisplay" => entry.no_display = parse_bool(&value).unwrap_or(false),
            // Unknown keys are ignored per the specification.
            _ => {}
        }
    }

    Ok(entry)
}

/// Parse a desktop entry boolean: `true`/`false` (case-insensitive) or `1`/`0`.
fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// Resolve the escape sequences from the desktop entry specification.
///
/// Unknown escapes are kept verbatim (`\q` becomes the two characters `\q`),
/// a trailing backslash is kept, and any other character passes through.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();

    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_exec_and_icon() {
        let contents = "[Desktop Entry]\nType=Application\nName=Terminal\nExec=kgx %U\nIcon=utilities-terminal\n";
        let entry = parse(contents).expect("valid entry should parse");
        assert_eq!(entry.name.as_deref(), Some("Terminal"));
        assert_eq!(entry.exec.as_deref(), Some("kgx %U"));
        assert_eq!(entry.icon.as_deref(), Some("utilities-terminal"));
        assert_eq!(entry.entry_type.as_deref(), Some("Application"));
        assert!(!entry.no_display);
    }

    #[test]
    fn ignores_comments_blank_lines_and_crlf() {
        let contents = "# comment\r\n\r\n[Desktop Entry]\r\n# another comment\r\nName=Files\r\n";
        let entry = parse(contents).expect("should parse");
        assert_eq!(entry.name.as_deref(), Some("Files"));
    }

    #[test]
    fn keeps_exec_quotes_and_placeholders() {
        let contents = "[Desktop Entry]\nType=Application\nName=Editor\nExec=\"/usr/bin/code --diff\" %F\n";
        let entry = parse(contents).expect("should parse");
        assert_eq!(entry.exec.as_deref(), Some("\"/usr/bin/code --diff\" %F"));
    }

    #[test]
    fn prefers_bare_key_over_localized_variants() {
        let contents = concat!(
            "[Desktop Entry]\n",
            "Type=Application\n",
            "Name=Fallback\n",
            "Name[de]=Fallback (de)\n",
        );
        let entry = parse(contents).expect("should parse");
        assert_eq!(entry.name.as_deref(), Some("Fallback"));
    }

    #[test]
    fn parses_booleans_case_insensitively() {
        for value in ["true", "TRUE", "True", "1"] {
            let contents = format!("[Desktop Entry]\nType=Application\nName=App\nNoDisplay={value}\n");
            let entry = parse(&contents).unwrap();
            assert!(entry.no_display, "NoDisplay={value} should be true");
        }
        for value in ["false", "FALSE", "0"] {
            let contents = format!("[Desktop Entry]\nType=Application\nName=App\nNoDisplay={value}\n");
            let entry = parse(&contents).unwrap();
            assert!(!entry.no_display, "NoDisplay={value} should be false");
        }
        // A non-boolean value must not match the `true` branch.
        let contents = "[Desktop Entry]\nType=Application\nName=App\nNoDisplay=maybe\n";
        let entry = parse(contents).unwrap();
        assert!(!entry.no_display);
    }

    #[test]
    fn resolves_escape_sequences_in_values() {
        let contents = "[Desktop Entry]\nType=Application\nName=Line\\nTwo\tAnd\\sIn\\t\n";
        let entry = parse(contents).unwrap();
        assert_eq!(entry.name.as_deref(), Some("Line\nTwo\tAnd In\t"));
    }

    #[test]
    fn ignores_unrelated_keys_and_other_groups() {
        let contents = concat!(
            "[Desktop Entry]\n",
            "Type=Application\n",
            "Name=App\n",
            "Categories=Utility;Development;\n",
            "Keywords=test;fixture;\n",
            "X-Custom=anything\n",
            "[Desktop Action undo]\n",
            "Name=Undo\n",
            "Exec=undo\n",
        );
        let entry = parse(contents).unwrap();
        assert_eq!(entry.name.as_deref(), Some("App"));
        assert!(entry.exec.is_none(), "Exec from another group must be ignored");
    }

    #[test]
    fn rejects_malformed_group_headers() {
        let contents = "[Desktop Entry\nType=Application\n";
        assert!(matches!(parse(contents), Err(ParseError::MalformedGroup { .. })));
    }

    #[test]
    fn rejects_content_outside_any_group() {
        let contents = "Type=Application\n[Desktop Entry]\nName=App\n";
        assert!(matches!(parse(contents), Err(ParseError::ContentOutsideGroup { .. })));
    }

    #[test]
    fn rejects_key_lines_without_equals() {
        let contents = "[Desktop Entry]\nType=Application\nName App\n";
        assert!(matches!(parse(contents), Err(ParseError::MissingEqual { .. })));
    }
}