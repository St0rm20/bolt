//! Launching applications from the `Exec` field of a `.desktop` entry.
//!
//! Desktop entries store an `Exec` string that is *not* a shell command line:
//! quoting rules are simpler than a shell's, and `%`-field codes (`%U`, `%F`,
//! `%f`, `%i`, ...) must be substituted or dropped. Keeping the preparation
//! here — away from the GTK UI — makes it unit-testable and lets a future
//! phase (e.g. launching with files or URLs) change the behaviour without
//! touching widget code.
//!
//! [`build_command`] parses a raw `Exec` line into a concrete program plus
//! arguments, and [`launch`] spawns that program. Launching never waits for
//! the child process, so the caller (the GTK main thread) is never blocked.

use std::io;
use std::process::Command as ProcessCommand;

/// Field codes defined by the desktop entry specification that expand to
/// things we do not have (files, URLs, the icon name, the translated name,
/// the desktop file path, ...). Whenever a launch has no files or URLs, the
/// specification says these should be removed from the command line.
const DROPPED_FIELD_CODES: &[char] =
    &['f', 'F', 'u', 'U', 'd', 'D', 'n', 'N', 'v', 'm', 'k', 'c', 'i'];

/// A fully prepared launch: executable plus arguments, safe to hand to
/// [`ProcessCommand`] without any shell in between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCommand {
    /// Executable (single binary on `PATH`, or an absolute path).
    pub program: String,
    /// Arguments, field codes already removed and escapes resolved.
    pub args: Vec<String>,
}

/// Parse an `Exec` line into a [`LaunchCommand`].
///
/// Rules implemented (a pragmatic subset of the desktop entry spec):
/// - fields are separated on unquoted whitespace;
/// - double- and single-quoted segments group their contents (quotes are
///   removed, matching the spec's "quote" handling);
/// - `%%` is a literal percent sign;
/// - `%` followed by a known field code drops the *whole* two-character
///   sequence (no files/URLs are ever passed for now);
/// - any other `%` sequence is kept verbatim.
///
/// Returns `None` when the line contains no runnable command (empty input,
/// only dropped field codes, ...).
pub fn build_command(exec: &str) -> Option<LaunchCommand> {
    let mut fields = split_into_fields(exec)
        .into_iter()
        .map(|field| expand_fields(&field))
        .filter(|field| !field.is_empty());

    let program = fields.next()?;
    let args = fields.collect();
    Some(LaunchCommand { program, args })
}

/// Spawn the application described by a raw `Exec` line.
///
/// The process is spawned and immediately handed back; it is never waited on,
/// so this call returns as soon as the child has been started (or such a
/// start failed). Errors such as a missing executable are returned to the
/// caller to report, they never abort the launcher.
pub fn launch(exec: &str) -> io::Result<()> {
    let command = match build_command(exec) {
        Some(command) => command,
        None => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Exec line contains no runnable command: {exec:?}"),
            ));
        }
    };

    let mut process = ProcessCommand::new(&command.program);
    process.args(&command.args);
    match process.spawn() {
        Ok(_) => Ok(()),
        Err(err) => Err(err),
    }
}

/// Split an `Exec` line into fields, honouring double and single quotes.
fn split_into_fields(exec: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_field = false;
    let mut quote: Option<char> = None;

    for ch in exec.chars() {
        match quote {
            Some(quote_char) => {
                if ch == quote_char {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            None => {
                if ch == '"' || ch == '\'' {
                    quote = Some(ch);
                    in_field = true;
                } else if ch.is_whitespace() {
                    if in_field {
                        fields.push(std::mem::take(&mut current));
                    }
                    in_field = false;
                } else {
                    current.push(ch);
                    in_field = true;
                }
            }
        }
    }
    if in_field {
        fields.push(current);
    }
    fields
}

/// Remove dropped field codes and unfold `%%` inside a single field.
fn expand_fields(field: &str) -> String {
    let mut expanded = String::with_capacity(field.len());
    let characters: Vec<char> = field.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        let ch = characters[index];
        if ch == '%' && index + 1 < characters.len() {
            let next = characters[index + 1];
            if next == '%' {
                expanded.push('%');
                index += 2;
                continue;
            }
            if DROPPED_FIELD_CODES.contains(&next) {
                index += 2;
                continue;
            }
        }
        expanded.push(ch);
        index += 1;
    }
    expanded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(exec: &str) -> Option<(String, Vec<String>)> {
        build_command(exec).map(|c| (c.program, c.args))
    }

    #[test]
    fn strips_coalesced_fields_and_flags() {
        assert_eq!(command("firefox %u"), Some(("firefox".into(), vec![])));
        assert_eq!(command("code %F"), Some(("code".into(), vec![])));
        assert_eq!(
            command("/usr/bin/valid-app --flag %U"),
            Some(("/usr/bin/valid-app".into(), vec!["--flag".to_owned()]))
        );
        assert_eq!(command("kgx %U"), Some(("kgx".into(), vec![])));
    }

    #[test]
    fn keeps_unknown_percent_sequences() {
        assert_eq!(command("a %% b"), Some(("a".into(), vec!["%".into(), "b".into()])));
        assert_eq!(command("a %q b"), Some(("a".into(), vec!["%q".into(), "b".into()])));
        assert_eq!(command("--percent=50%"), Some(("--percent=50%".into(), vec![])));
    }

    #[test]
    fn removes_embedded_field_codes() {
        assert_eq!(command("--file=%U"), Some(("--file=".into(), vec![])));
        assert_eq!(
            command("sh -c 'cp %F /tmp'"),
            Some(("sh".into(), vec!["-c".into(), "cp  /tmp".into()]))
        );
    }

    #[test]
    fn handles_quotes_and_repeated_whitespace() {
        assert_eq!(
            command("App \"quoted arg\" 'another one'"),
            Some(("App".into(), vec!["quoted arg".into(), "another one".into()]))
        );
        assert_eq!(command("app   -o  "), Some(("app".into(), vec!["-o".into()])));
        assert_eq!(command("app \"\" tail"), Some(("app".into(), vec!["tail".into()])));
        assert_eq!(
            command("sh -c 'echo \"hi\"'"),
            Some(("sh".into(), vec!["-c".into(), "echo \"hi\"".into()]))
        );
    }

    #[test]
    fn unclosed_quotes_are_tolerated() {
        assert_eq!(command("app \"unterminated"), Some(("app".into(), vec!["unterminated".into()])));
    }

    #[test]
    fn empty_or_only_field_codes_yield_nothing() {
        assert_eq!(build_command(""), None);
        assert_eq!(build_command("   "), None);
        assert_eq!(build_command("%U"), None);
        assert_eq!(build_command("%F %u"), None);
        // A bare `&&` is not a field code: it is parsed literally.
        assert_eq!(command("%F &&"), Some(("&&".into(), vec![])));
    }

    #[test]
    fn launch_fails_cleanly_on_missing_executable() {
        // Never spawns anything: a non-existent binary fails immediately.
        let err = launch("/definitely/not/a/program-xyz-123").expect_err("should fail");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn launch_rejects_unrunnable_lines_without_panicking() {
        let err = launch("   ").expect_err("empty Exec should fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}