//! `clipboard` — the clipboard-history plugin.
//!
//! Activated by the `clip:` prefix (ASCII case-insensitive). `clip:` lists the
//! whole history (newest first); `clip:<text>` shows only entries containing
//! `<text>`, case-insensitively. Each row is a preview of the stored text with
//! the full original content kept in its [`PluginAction::Copy`], so Enter
//! writes the *complete* entry back to the system clipboard (through the GTK
//! layer's existing `PluginAction::Copy` handling — no shell involved).
//!
//! The plugin never reads the clipboard itself: it only queries a shared
//! [`ClipboardHistory`], which the daemon-owned [`ClipboardMonitor`] keeps
//! populated in the background. That keeps the plugin cheap (a substring scan
//! of at most [`ClipboardHistory::max_len`] strings per keystroke) and lets it
//! work even when live capture is unavailable — persisted history is loaded at
//! startup.

use crate::clipboard::history::{ClipboardEntry, ClipboardHistory};
use crate::{Plugin, PluginAction, PluginResult};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Identifier used by the `enabled_plugins` config allow-list.
pub const CLIPBOARD_PLUGIN_ID: &str = "clipboard";

/// Trigger prefix, e.g. `clip:github`.
pub const CLIPBOARD_PREFIX: &str = "clip:";

/// Maximum characters of an entry shown in the result title. Long clipboard
/// contents keep the full text in the copy action; only the display is
/// shortened, so widgets stay small.
pub const PREVIEW_MAX_CHARS: usize = 60;

/// The clipboard-history plugin.
///
/// Holds no clipboard backend — just a shared handle to the history the
/// monitor updates. `matches`/`query` are pure reads.
pub struct ClipboardPlugin {
    history: Arc<Mutex<ClipboardHistory>>,
}

impl ClipboardPlugin {
    /// Build a plugin over the shared history (usually also owned by the
    /// running [`ClipboardMonitor`](crate::clipboard::monitor::ClipboardMonitor)).
    #[must_use]
    pub fn new(history: Arc<Mutex<ClipboardHistory>>) -> Self {
        Self { history }
    }

    fn lock_history(&self) -> std::sync::MutexGuard<'_, ClipboardHistory> {
        self.history.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Plugin for ClipboardPlugin {
    fn id(&self) -> &str {
        CLIPBOARD_PLUGIN_ID
    }

    fn name(&self) -> &str {
        "Clipboard"
    }

    fn prefix(&self) -> Option<&str> {
        Some(CLIPBOARD_PREFIX)
    }

    fn query(&self, query: &str) -> Vec<PluginResult> {
        // Defensive gate: the launcher never calls `query` unless `matches` was
        // true, but a direct call with a query that doesn't start with the
        // prefix (any ASCII case) must yield nothing rather than slicing
        // `"2 + 2"` and searching the leftover.
        let has_prefix = query
            .get(..CLIPBOARD_PREFIX.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(CLIPBOARD_PREFIX));
        if !has_prefix {
            return Vec::new();
        }
        let Some(rest) = query.get(CLIPBOARD_PREFIX.len()..) else {
            return Vec::new();
        };
        // The prefix is removed by length (the ASCII prefix is fixed-width and
        // `matches` already gated this call on the query *starting* with it),
        // so it works whether the user typed `clip:` or `CLIP:`. The prefix
        // itself is excluded from the filter; leading/trailing whitespace the
        // user typed after it is ignored (`clip: rust` searches `rust`).
        let filter = rest.trim();
        let now = clock_now();
        let matches = self.lock_history().search_entries(filter);
        matches.iter().map(|entry| result_row(entry, now)).collect()
    }
}

/// One history entry as a launcher row: a previewed title (full text in the
/// copy action), a size + age hint as subtitle, and a copy action carrying
/// the complete original content.
fn result_row(entry: &ClipboardEntry, now: u64) -> PluginResult {
    let title = preview(&entry.text);
    let subtitle = format!(
        "{} characters · {}",
        entry.text.chars().count(),
        format_age(now, entry.timestamp)
    );
    PluginResult::with_subtitle(title, subtitle)
        .with_action(PluginAction::Copy { text: entry.text.clone() })
}

/// The current wall-clock time in whole seconds since the Unix epoch, used to
/// age the history entries shown by the plugin.
fn clock_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// A short human age for an entry captured `timestamp` seconds ago, e.g.
/// `just now`, `5m`, `3h`, `2d`, `6w` or `4mo`. Future timestamps (clock
/// skew) read as `just now`.
fn format_age(now: u64, timestamp: u64) -> String {
    let age = now.saturating_sub(timestamp);
    let minutes = age / 60;
    if minutes < 1 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 7 {
        return format!("{days}d");
    }
    let weeks = days / 7;
    if weeks < 5 {
        return format!("{weeks}w");
    }
    format!("{}mo", days / 30)
}

/// A single-line preview of `text` for display, at most
/// [`PREVIEW_MAX_CHARS`] characters.
///
/// Control characters (newlines, tabs, ...) collapse to spaces so a row never
/// renders multi-line, and over-long content is truncated with a trailing `…`.
/// The returned string is for display only — the action keeps the original.
pub fn preview(text: &str) -> String {
    let flat = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let mut shortened: String = flat.chars().take(PREVIEW_MAX_CHARS).collect();
    if flat.chars().count() > PREVIEW_MAX_CHARS {
        shortened.push('…');
    }
    shortened
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::history::ClipboardHistory;
    use crate::{PluginRegistry};

    fn plugin_with(entries: &[&str]) -> ClipboardPlugin {
        let mut history = ClipboardHistory::new();
        for entry in entries {
            history.add(entry);
        }
        ClipboardPlugin::new(Arc::new(Mutex::new(history)))
    }

    fn titles(plugin: &dyn Plugin, query: &str) -> Vec<PluginResult> {
        plugin.query(query)
    }

    #[test]
    fn clip_colon_lists_the_whole_history_newest_first() {
        let plugin = plugin_with(&["old", "mid", "new"]);
        // Entries end up newest-first: add("old"), add("mid"), add("new").
        let rows = plugin.query("clip:");
        let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, ["new", "mid", "old"]);
    }

    #[test]
    fn clip_text_filters_entries() {
        let plugin = plugin_with(&["rust bindings", "web dev", "rust playground", "terminal"]);
        let rows = titles(&plugin, "clip:rust");
        let titles: Vec<&str> = rows.iter().map(|row| row.title.as_str()).collect();
        assert_eq!(titles, ["rust playground", "rust bindings"]);
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn filtering_is_case_insensitive() {
        let plugin = plugin_with(&["Github repo", "git notes", "other"]);
        let rows = plugin.query("clip:gITHub");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Github repo");
        let rows = plugin.query("CLIP:REPO");
        assert_eq!(rows.len(), 1, "the prefix itself is case-insensitive too");
    }

    #[test]
    fn the_prefix_is_excluded_from_the_filter() {
        // Bare "clip:" must list everything, even when an entry's text itself
        // contains the literal "clip:" — the prefix must not leak into the
        // filter and turn the listing into a search for "clip:".
        let plugin = plugin_with(&["clip: itself", "plain text"]);
        let rows = plugin.query("clip:");
        assert_eq!(rows.len(), 2, "bare clip: shows the whole history");

        // Searching "rust" matches the word wherever it appears (including in
        // an entry whose text *is* "clip: rust ..."); the prefix never gets
        // appended to the filter.
        let plugin = plugin_with(&["clip: rust is text", "I use rust daily"]);
        let rows = plugin.query("clip:rust");
        assert_eq!(rows.len(), 2, "both entries contain the word rust");
    }

    #[test]
    fn normal_queries_do_not_activate_the_plugin() {
        let plugin = plugin_with(&["anything"]);
        for query in ["firefox", "calculator", "2 + 2", "clip", "cliphistory", "web"] {
            assert!(!plugin.matches(query), "{query:?} must not match");
            assert!(plugin.query(query).is_empty(), "{query:?} must yield no rows");
        }
        assert!(plugin.matches("clip:"));
        assert!(plugin.matches("CLIP:x"));
    }

    #[test]
    fn empty_history_yields_no_rows() {
        let plugin = plugin_with(&[]);
        assert!(plugin.query("clip:").is_empty());
        assert!(plugin.query("clip:anything").is_empty());
    }

    #[test]
    fn every_row_carries_a_copy_action_with_the_full_original() {
        let long = "line one\nline two\n".repeat(200);
        let plugin = plugin_with(&[&long]);
        let rows = plugin.query("clip:");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].action,
            Some(PluginAction::Copy { text: long.clone() }),
            "the action preserves the complete multi-line content"
        );
        // The visible title is a short, single-line preview.
        assert_ne!(rows[0].title, long);
        assert!(rows[0].title.chars().count() <= PREVIEW_MAX_CHARS + 1, "preview + ellipsis");
        assert!(!rows[0].title.contains('\n'), "preview never spans lines");
    }

    #[test]
    fn long_visible_preview_is_truncated_with_an_ellipsis() {
        let content = "abcdefghij".repeat(10); // 100 chars
        let previewed = preview(&content);
        assert_eq!(previewed.chars().count(), PREVIEW_MAX_CHARS + 1);
        assert!(previewed.ends_with('…'));
    }

    #[test]
    fn short_content_preview_is_left_untouched() {
        assert_eq!(preview("hello world"), "hello world");
        assert_eq!(preview(""), "");
    }

    #[test]
    fn control_characters_collapse_to_spaces_in_the_preview() {
        let previewed = preview("a\nb\tc\rd");
        assert!(!previewed.contains('\n'));
        assert!(!previewed.contains('\t'));
        assert!(!previewed.contains('\r'));
    }

    #[test]
    fn subtitle_reports_content_length_and_age() {
        let plugin = plugin_with(&["abc"]);
        let rows = plugin.query("clip:");
        let subtitle = rows[0].subtitle.as_deref().expect("a subtitle");
        assert!(subtitle.starts_with("3 characters · "), "subtitle: {subtitle}");
        assert!(
            subtitle.ends_with("just now"),
            "a fresh copy reports its age: {subtitle}"
        );
    }

    #[test]
    fn format_age_renders_human_durations() {
        // A mid-century base timestamp leaves plenty of headroom for the
        // multi-day subtractions below without overflowing.
        let now = 4_000_000_000u64;
        assert_eq!(format_age(now, now), "just now");
        assert_eq!(format_age(now, now - 30), "just now");
        assert_eq!(format_age(now, now - 5 * 60), "5m");
        assert_eq!(format_age(now, now - 3 * 3_600), "3h");
        assert_eq!(format_age(now, now - 2 * 86_400), "2d");
        assert_eq!(format_age(now, now - 3 * 7 * 86_400), "3w");
        // Weeks run 1-4; five weeks and up read as whole months.
        assert_eq!(format_age(now, now - 35 * 86_400), "1mo");
        assert_eq!(format_age(now, now - 65 * 86_400), "2mo");
        assert_eq!(format_age(now + 60, now), "1m", "a minute ago reads as 1m");
        assert_eq!(format_age(now, now + 60), "just now", "future timestamps count as fresh");
    }

    #[test]
    fn dispatches_through_the_registry_like_every_plugin() {
        let history = Arc::new(Mutex::new(ClipboardHistory::new()));
        history.lock().unwrap().add("registry entry");
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(ClipboardPlugin::new(history.clone())));
        let (index, rows) = registry.dispatch("clip:").expect("the plugin matches");
        assert_eq!(index, 0);
        assert_eq!(rows[0].title, "registry entry");
        assert_eq!(registry.dispatch("firefox"), None, "apps stay untouched");
    }
}