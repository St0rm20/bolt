//! Line-based IPC protocol shared by the daemon socket and the CLI client.
//!
//! Each request is a single line naming a command; each response is a single
//! line, either `OK` or `ERR <detail>`.

use std::fmt;

/// Commands that can be sent to the running daemon over its Unix socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Show the launcher window if hidden, hide it if visible.
    Toggle,
    /// Show and focus the launcher window.
    Show,
    /// Hide the launcher window.
    Hide,
    /// Ask the daemon to shut down cleanly.
    Quit,
}

impl Command {
    /// Parse a single trimmed protocol line into a [`Command`].
    ///
    /// Matching is case-insensitive so both `TOGGLE` and `toggle` work.
    pub fn parse(line: &str) -> Result<Self, &'static str> {
        match line.trim().to_ascii_uppercase().as_str() {
            "TOGGLE" => Ok(Command::Toggle),
            "SHOW" => Ok(Command::Show),
            "HIDE" => Ok(Command::Hide),
            "QUIT" => Ok(Command::Quit),
            _ => Err("unknown command (expected TOGGLE, SHOW, HIDE or QUIT)"),
        }
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wire = match self {
            Command::Toggle => "TOGGLE",
            Command::Show => "SHOW",
            Command::Hide => "HIDE",
            Command::Quit => "QUIT",
        };
        f.write_str(wire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_commands_case_insensitively() {
        for command in [Command::Toggle, Command::Show, Command::Hide, Command::Quit] {
            let lower = command.to_string().to_ascii_lowercase();
            assert_eq!(Command::parse(&lower), Ok(command));
        }
        assert_eq!(Command::parse("  TOGGLE  "), Ok(Command::Toggle));
    }

    #[test]
    fn rejects_unknown_commands() {
        assert!(Command::parse("BOGUS").is_err());
        assert!(Command::parse("").is_err());
    }
}