//! `echo` — the minimal example plugin.
//!
//! Registered by the daemon at startup, it validates the plugin pipeline end
//! to end: typing `echo:` in the launcher activates the plugin and the
//! query's remainder is echoed back as a result row.

use crate::{Plugin, PluginResult};

/// Identifier used by the `enabled_plugins` config allow-list.
pub const ECHO_PLUGIN_ID: &str = "echo";

/// Trigger prefix, e.g. `echo: hello`.
pub const ECHO_PREFIX: &str = "echo:";

/// The example plugin: returns `echo: <text>` for any query with the prefix.
#[derive(Debug, Clone, Copy, Default)]
pub struct EchoPlugin;

impl Plugin for EchoPlugin {
    fn id(&self) -> &str {
        ECHO_PLUGIN_ID
    }

    fn name(&self) -> &str {
        "Echo"
    }

    fn prefix(&self) -> Option<&str> {
        Some(ECHO_PREFIX)
    }

    fn query(&self, query: &str) -> Vec<PluginResult> {
        // The prefix is ASCII; slicing at its byte length is always a char
        // boundary because `matches` verified the query starts with it.
        let Some(text) = query.get(ECHO_PREFIX.len()..) else {
            return Vec::new();
        };
        let text = text.trim();
        if text.is_empty() {
            vec![PluginResult::new("type something after \"echo:\"")]
        } else {
            vec![PluginResult::new(format!("echo: {text}"))]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activates_only_on_the_prefix() {
        assert!(EchoPlugin.matches("echo: hello"));
        assert!(EchoPlugin.matches("ECHO: hello"), "case-insensitive prefix");
        assert!(!EchoPlugin.matches("echo"), "missing the colon");
        assert!(!EchoPlugin.matches("firefox"));
        assert!(!EchoPlugin.matches(" echo: hello"));
    }

    #[test]
    fn echoes_the_trimmed_remainder() {
        let results = EchoPlugin.query("echo: hello");
        assert_eq!(results, vec![PluginResult::new("echo: hello")]);
    }

    #[test]
    fn ignores_leading_whitespace_after_the_prefix() {
        let results = EchoPlugin.query("echo:   hello world  ");
        assert_eq!(results, vec![PluginResult::new("echo: hello world")]);
    }

    #[test]
    fn empty_remainder_returns_a_hint() {
        let results = EchoPlugin.query("echo:");
        assert_eq!(results, vec![PluginResult::new("type something after \"echo:\"")]);
    }
}