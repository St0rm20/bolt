//! Bounded clipboard history: the pure, GTK-free data model the clipboard
//! plugin and monitor share.
//!
//! [`ClipboardHistory`] owns the ordered entry list (newest first), dedupes
//! entries, filters them for the `clip:` query and round-trips itself to a
//! JSON file of `{"text", "timestamp"}` objects (newest first). Persistence is a thin slice
//! of [`std::fs`] with an atomic replace (write to a sibling temp file, then
//! `rename`) so an interrupted write never corrupts the live file.
//!
//! The type is deliberately independent of GTK *and* of the clipboard backend:
//! everything is testable with plain in-memory strings. Entries carry capture
//! timestamps (seconds since the Unix epoch) so an optional retention can
//! prune by age and the `clip:` plugin can show how long ago something was
//! copied. The dedup rule (drop consecutive duplicates, move re-copies to the
//! front) matches typical clipboard-manager behaviour.

use std::collections::VecDeque;
use std::fs;
use std::io::Result as IoResult;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default bound on the number of entries kept in memory and on disk.
pub const DEFAULT_HISTORY_LIMIT: usize = 50;

/// File name of the persisted history under the launcher's XDG data dir.
pub const HISTORY_FILE_NAME: &str = "clipboard_history.json";

/// Bounded, newest-first list of clipboard texts.
///
/// Index `0` is the most recent copy. `add` never stores the empty string and
/// never stores a consecutive duplicate; a duplicate of an older entry is
/// moved to the front instead of being added twice. Entries carry capture
/// timestamps and are pruned by age when a retention is set (see
/// [`ClipboardHistory::set_retention_seconds`]).
#[derive(Debug, Clone)]
pub struct ClipboardHistory {
    entries: VecDeque<ClipboardEntry>,
    max_len: usize,
    retention_seconds: Option<u64>,
}

/// One clipboard entry: the copied text and when it was captured (seconds
/// since the Unix epoch). The timestamp drives retention-based pruning and
/// the plugin's "copied X ago" age hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardEntry {
    /// The copied text, verbatim.
    pub text: String,
    /// When the entry was recorded, seconds since the Unix epoch.
    pub timestamp: u64,
}

/// The current wall-clock time in whole seconds since the Unix epoch, used
/// for capture timestamps and retention pruning. `0` only if the clock is
/// before the epoch — never in practice.
fn clock_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// What [`load_into`] found on disk.
#[derive(Debug)]
pub enum LoadOutcome {
    /// A valid file was loaded; carries the resulting entry count.
    Loaded(usize),
    /// No file exists yet — an empty history is normal for a first run.
    Missing,
    /// The file could not be parsed as JSON (or was not a string array).
    /// The contents are ignored; nothing is changed.
    Corrupted,
    /// The file exists but could not be read.
    Unreadable(std::io::Error),
}

impl ClipboardHistory {
    /// An empty history bounded to [`DEFAULT_HISTORY_LIMIT`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_limit(DEFAULT_HISTORY_LIMIT)
    }

    /// An empty history with a custom bound. `max_len` is at least 1, so a
    /// zero/`0` config value cannot accidentally disable capture silently.
    #[must_use]
    pub fn with_limit(max_len: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            max_len: max_len.max(1),
            retention_seconds: None,
        }
    }

    /// The configured bound.
    #[must_use]
    pub fn max_len(&self) -> usize {
        self.max_len
    }

    /// The retention duration in seconds, if any. `None` keeps the history
    /// memory-only — no age pruning and nothing persisted by the monitor.
    #[must_use]
    pub fn retention_seconds(&self) -> Option<u64> {
        self.retention_seconds
    }

    /// Set the retention duration in whole seconds. `None` disables age
    /// pruning (a session-only, in-memory history). Setting a value prunes
    /// entries that already exceed it.
    pub fn set_retention_seconds(&mut self, retention: Option<u64>) {
        self.retention_seconds = retention;
        if retention.is_some() {
            self.prune();
        }
    }

    /// Drop entries older than the configured retention. No-op without one.
    /// Pruning happens lazily on every read/write path, so expired entries
    /// never linger past the next access.
    pub fn prune(&mut self) {
        self.prune_at(clock_now());
    }

    fn prune_at(&mut self, now: u64) {
        let Some(retention) = self.retention_seconds else { return };
        self.entries
            .retain(|entry| entry.timestamp.saturating_add(retention) > now);
    }

    /// Number of stored entries (never exceeds [`Self::max_len`]).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the history holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record a copied text, newest first.
    ///
    /// Returns `true` when the history changed in a way worth persisting:
    /// a brand-new entry was inserted, or an older duplicate was moved to the
    /// front. Returns `false` when nothing changed — the empty string, or the
    /// same text already leading the history.
    ///
    /// The exact copied text is preserved (no trimming). On duplicate removal
    /// the previous occurrence is dropped and the text put back at the front.
    pub fn add(&mut self, text: &str) -> bool {
        self.add_at(text, clock_now())
    }

    /// Record a copied text with an explicit capture timestamp (used when
    /// re-hydrating persisted entries and in deterministic tests).
    pub fn add_at(&mut self, text: &str, timestamp: u64) -> bool {
        self.prune();
        if text.is_empty() {
            return false;
        }
        if self.entries.front().is_some_and(|front| front.text == text) {
            return false;
        }
        if let Some(position) = self.entries.iter().position(|entry| entry.text == text) {
            self.entries.remove(position);
        }
        self.entries.push_front(ClipboardEntry {
            text: text.to_owned(),
            timestamp,
        });
        while self.entries.len() > self.max_len {
            self.entries.pop_back();
        }
        true
    }

    /// The entry at `index` (0 = most recent), if any.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&str> {
        self.entries.get(index).map(|entry| entry.text.as_str())
    }

    /// All entries, most recent first.
    pub fn entries(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.text.as_str())
    }

    /// Entries containing `query`, case-insensitively, newest first.
    ///
    /// An empty query matches everything. The lookup is a plain substring
    /// scan — 50 short strings per keystroke, cheap enough to run while the
    /// user types. Returns *owned* strings so callers don't have to keep the
    /// history's lock alive. Expired entries are pruned first.
    #[must_use]
    pub fn search(&mut self, query: &str) -> Vec<String> {
        self.search_entries(query)
            .into_iter()
            .map(|entry| entry.text)
            .collect()
    }

    /// Like [`Self::search`], but returns the full entries (text + capture
    /// timestamp) so callers can display how old a result is.
    #[must_use]
    pub fn search_entries(&mut self, query: &str) -> Vec<ClipboardEntry> {
        self.prune();
        let needle = query.to_lowercase();
        self.entries
            .iter()
            .filter(|entry| entry.text.to_lowercase().contains(&needle))
            .cloned()
            .collect()
    }

    /// The clipboard sink (write target) associated with this history.
    ///
    /// The GTK window writes copy actions (e.g. `PluginAction::Copy`) to it.
    /// The real system clipboard is owned by the window's display connection,
    /// so this placeholder holds that reference's place in the pure, GTK-free
    /// model layer; the window shares its own handle instead. `()` keeps the
    /// history testable without any clipboard object.
    #[must_use]
    pub fn sink() -> () {
        ()
    }

    /// Drop every entry.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Serialise the history to the persistence format: a JSON array of
    /// `{"text", "timestamp"}` objects, newest first. `VecDeque` serialisation
    /// cannot fail.
    fn to_json(&self) -> String {
        let value = serde_json::json!(self
            .entries
            .iter()
            .map(|entry| serde_json::json!({ "text": entry.text, "timestamp": entry.timestamp }))
            .collect::<Vec<_>>());
        serde_json::to_string(&value).expect("serializing clipboard history cannot fail")
    }

    /// Replace the contents from persisted JSON. `false` when the text is not
    /// a well-formed history file — the history is then left as-is.
    ///
    /// Two formats are accepted: the current object form
    /// (`[{"text": "…", "timestamp": 1710000000}, …]`) and the legacy string
    /// array (`["…", …]`), whose entries are treated as freshly copied.
    pub fn from_json(&mut self, json: &str) -> bool {
        let parsed = match serde_json::from_str(json) {
            Ok(parsed) => parsed,
            Err(_) => return false,
        };
        let now = clock_now();
        let entries: Vec<ClipboardEntry> = match parsed {
            serde_json::Value::Array(items) => {
                let mut entries = Vec::with_capacity(items.len());
                for item in items {
                    let entry = match item {
                        serde_json::Value::String(text) => ClipboardEntry {
                            text,
                            timestamp: now,
                        },
                        serde_json::Value::Object(fields) => {
                            let Some(text) = fields.get("text").and_then(|value| value.as_str())
                            else {
                                return false;
                            };
                            let timestamp = fields
                                .get("timestamp")
                                .and_then(|value| value.as_u64())
                                .unwrap_or(now);
                            ClipboardEntry {
                                text: text.to_owned(),
                                timestamp,
                            }
                        }
                        _ => return false,
                    };
                    entries.push(entry);
                }
                entries
            }
            _ => return false,
        };
        self.entries.clear();
        self.entries
            .extend(entries.into_iter().filter(|entry| !entry.text.is_empty()));
        while self.entries.len() > self.max_len {
            self.entries.pop_back();
        }
        self.prune_at(now);
        true
    }

    /// Persist the history atomically (temp file + rename).
    ///
    /// The parent directory is created when missing. Best-effort `sync_all`
    /// before the rename keeps an interrupted write from leaving a torn file
    /// at the final path.
    pub fn save(&self, path: &Path) -> IoResult<()> {
        write_atomically(path, self.to_json().as_bytes())
    }
}

impl Default for ClipboardHistory {
    fn default() -> Self {
        Self::new()
    }
}

/// The launcher's XDG data directory (`$XDG_DATA_HOME/launcher`, falling back
/// to `~/.local/share/launcher` when `XDG_DATA_HOME` is unset or empty).
#[must_use]
pub fn default_data_dir() -> PathBuf {
    match std::env::var("XDG_DATA_HOME") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir).join("launcher"),
        _ => match std::env::var("HOME") {
            Ok(home) if !home.trim().is_empty() => PathBuf::from(home).join(".local/share/launcher"),
            _ => PathBuf::from(".local/share/launcher"),
        },
    }
}

/// Default persistence path: [`default_data_dir`]/[`HISTORY_FILE_NAME`].
#[must_use]
pub fn default_history_path() -> PathBuf {
    default_data_dir().join(HISTORY_FILE_NAME)
}

/// Load persisted history (if any) into `history`, so `clip:` shows past
/// copies immediately. Failures are reported, never fatal.
pub fn load_into(history: &mut ClipboardHistory, path: &Path) -> LoadOutcome {
    match fs::read_to_string(path) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => LoadOutcome::Missing,
        Err(err) => LoadOutcome::Unreadable(err),
        Ok(contents) => {
            if history.from_json(&contents) {
                LoadOutcome::Loaded(history.len())
            } else {
                LoadOutcome::Corrupted
            }
        }
    }
}

/// Write `contents` to `path` atomically: full write to a sibling temp file,
/// then `rename` over the target. `create_dir_all` covers a missing parent.
fn write_atomically(path: &Path, contents: &[u8]) -> IoResult<()> {
    let temp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(err) = fs::write(&temp, contents) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    if let Ok(file) = fs::File::open(&temp) {
        let _ = file.sync_all();
    }
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            let _ = fs::remove_file(&temp);
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throw-away temp directory unique to one test (`tag`), so parallel
    /// tests never share files.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bolt-clipboard-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn history(entries: &[&str]) -> ClipboardHistory {
        let mut history = ClipboardHistory::new();
        for entry in entries {
            history.add(entry);
        }
        history
    }

    #[test]
    fn adding_entries_puts_newest_first() {
        let mut history = ClipboardHistory::new();
        assert!(history.add("a"));
        assert!(history.add("b"));
        assert!(history.add("c"));
        let entries: Vec<&str> = history.entries().collect();
        assert_eq!(entries, ["c", "b", "a"]);
        assert_eq!(history.get(0), Some("c"));
        assert_eq!(history.get(2), Some("a"));
        assert_eq!(history.get(3), None);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn history_is_limited_to_50_entries() {
        let mut history = ClipboardHistory::new();
        for i in 0..55 {
            history.add(&format!("copy {i}"));
        }
        assert_eq!(history.len(), DEFAULT_HISTORY_LIMIT);
        assert_eq!(history.get(0), Some("copy 54"), "newest kept");
        assert_eq!(history.get(49), Some("copy 5"), "oldest kept is the 50th");
        assert_eq!(history.get(50), None, "copy 4 .. copy 0 are gone");
    }

    #[test]
    fn consecutive_duplicates_are_not_stored() {
        let mut history = ClipboardHistory::new();
        assert!(history.add("same"));
        assert!(!history.add("same"), "consecutive duplicate is a no-op");
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn repeated_content_moves_to_the_front() {
        let mut history = history(&["a", "b", "c"]);
        assert!(history.add("a"), "a re-copy is a real change");
        let entries: Vec<&str> = history.entries().collect();
        assert_eq!(entries, ["a", "c", "b"], "a moved to the front, not duplicated");
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn empty_and_blank_text_is_not_stored() {
        let mut history = ClipboardHistory::new();
        assert!(!history.add(""));
        assert_eq!(history.len(), 0);
        assert!(history.add("x"));
        assert!(!history.add(""), "empty never enters the history");
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn empty_history_is_empty() {
        let mut history = ClipboardHistory::new();
        assert!(history.is_empty());
        assert_eq!(history.len(), 0);
        assert!(history.entries().next().is_none());
        assert!(history.get(0).is_none());
        history.add("x");
        history.clear();
        assert!(history.is_empty());
        assert!(history.entries().next().is_none());
    }

    #[test]
    fn empty_history_is_limited_to_2_entries() {
        let mut history = ClipboardHistory::with_limit(2);
        history.add("a");
        history.add("b");
        history.add("c");
        assert_eq!(history.entries().collect::<Vec<_>>(), ["c", "b"]);
    }

    #[test]
    fn search_filters_substrings_preserving_order() {
        let mut history = history(&["rust bindings", "web dev", "Rust playground", "terminal"]);
        assert_eq!(history.search("rust"), ["Rust playground", "rust bindings"]);
    }

    #[test]
    fn search_is_case_insensitive() {
        // add() keeps newest-first, so the internal order is:
        // ["ugol-rest", "GOLang", "Rust Lang"].
        let mut history = history(&["Rust Lang", "GOLang", "ugol-rest"]);
        assert_eq!(history.search("RUST"), ["Rust Lang"]);
        assert_eq!(history.search("go"), ["ugol-rest", "GOLang"]);
        assert_eq!(history.search("gO"), ["ugol-rest", "GOLang"]);
    }

    #[test]
    fn search_with_an_empty_query_matches_everything() {
        // add() keeps newest-first, so the order is ["b", "a"].
        let mut history = history(&["a", "b"]);
        assert_eq!(history.search(""), ["b", "a"]);
    }

    #[test]
    fn long_contents_are_stored_completely() {
        let long = "x".repeat(100_000);
        let mut history = ClipboardHistory::new();
        assert!(history.add(&long));
        assert_eq!(history.get(0), Some(long.as_str()));
        assert_eq!(history.entries().collect::<Vec<_>>(), [long.as_str()]);
    }

    #[test]
    fn unicode_text_round_trips_through_search() {
        let mut history = ClipboardHistory::new();
        history.add("contraseña");
        history.add("clave secreta");
        assert_eq!(history.search("contraseña"), ["contraseña"]);
        assert_eq!(history.search("SECR"), ["clave secreta"]);
    }

    #[test]
    fn save_then_load_round_trips() {
        let path = temp_dir("roundtrip").join(HISTORY_FILE_NAME);
        // add() keeps newest-first: ["first", "second", "third"].
        let history = history(&["third", "second", "first"]);
        history.save(&path).expect("save succeeds");
        let mut loaded = ClipboardHistory::new();
        assert!(matches!(load_into(&mut loaded, &path), LoadOutcome::Loaded(3)));
        assert_eq!(loaded.entries().collect::<Vec<_>>(), ["first", "second", "third"]);
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let dir = temp_dir("mkdirs");
        let path = dir.join("nested/deeper").join(HISTORY_FILE_NAME);
        history(&["a"]).save(&path).expect("parent dirs are created");
        assert!(path.exists());
    }

    #[test]
    fn missing_file_reports_a_missing_outcome_and_changes_nothing() {
        let path = temp_dir("missing").join(HISTORY_FILE_NAME);
        let mut loaded = history(&["a"]);
        assert!(matches!(load_into(&mut loaded, &path), LoadOutcome::Missing));
        assert_eq!(loaded.entries().collect::<Vec<_>>(), ["a"], "load failure never wipes memory");
    }

    #[test]
    fn corrupted_file_reports_corrupted_without_crashing() {
        let path = temp_dir("corrupt").join(HISTORY_FILE_NAME);
        fs::write(&path, "{ not json, [1, 2] }").expect("write");
        let mut loaded = history(&["keepme"]);
        assert!(matches!(load_into(&mut loaded, &path), LoadOutcome::Corrupted));
        assert_eq!(loaded.entries().collect::<Vec<_>>(), ["keepme"]);
    }

    #[test]
    fn non_string_json_is_rejected() {
        let mut history = ClipboardHistory::new();
        assert!(!history.from_json("\"just a string\""));
        assert!(!history.from_json("[1, 2, 3]"));
        assert!(!history.from_json("null"));
        assert!(!history.from_json(""));
        assert!(history.is_empty());
    }

    #[test]
    fn loaded_history_is_clamped_to_the_limit() {
        let path = temp_dir("clamp").join(HISTORY_FILE_NAME);
        let mut sixty = ClipboardHistory::with_limit(100);
        for i in 0..60 {
            sixty.add(&format!("entry {i}"));
        }
        sixty.save(&path).expect("save");

        let mut history = ClipboardHistory::with_limit(10);
        assert!(matches!(load_into(&mut history, &path), LoadOutcome::Loaded(10)));
        assert_eq!(history.get(0), Some("entry 59"), "most recent kept");
        assert_eq!(history.get(9), Some("entry 50"));
        assert_eq!(history.len(), 10);
    }

    #[test]
    fn persisted_limit_is_never_exceeded_on_load() {
        let mut history = ClipboardHistory::with_limit(DEFAULT_HISTORY_LIMIT);
        for i in 0..(DEFAULT_HISTORY_LIMIT * 2) {
            history.add(&format!("entry {i}"));
        }
        assert_eq!(history.len(), DEFAULT_HISTORY_LIMIT);
        let json = history.to_json();
        let mut reloaded = ClipboardHistory::new();
        assert!(reloaded.from_json(&json));
        assert!(reloaded.len() <= DEFAULT_HISTORY_LIMIT);
    }

    #[test]
    fn save_then_load_on_small_limit_evicts_oldest_only() {
        let path = temp_dir("replace").join(HISTORY_FILE_NAME);
        let mut history = ClipboardHistory::with_limit(3);
        for entry in ["a", "b", "c"] {
            history.add(entry);
        }
        history.save(&path).expect("save");
        history.add("d");
        history.save(&path).expect("second save replaces the file");

        let mut loaded = ClipboardHistory::new();
        assert!(matches!(load_into(&mut loaded, &path), LoadOutcome::Loaded(_)));
        assert_eq!(loaded.entries().collect::<Vec<_>>(), ["d", "c", "b"]);
    }

    #[test]
    fn empty_entries_are_skipped_while_loading() {
        let mut history = ClipboardHistory::new();
        assert!(history.from_json(r#"["a", "", "b", ""]"#));
        assert_eq!(history.entries().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn default_history_path_is_under_the_launcher_data_dir() {
        // The exact value depends on the environment's XDG/HOME vars; the
        // contract is only the launcher dir and file name.
        let path = default_data_dir();
        assert!(path.file_name().is_some_and(|name| name == "launcher"));
        assert_eq!(default_history_path().file_name(), Some(std::ffi::OsStr::new(HISTORY_FILE_NAME)));
    }

    #[test]
    fn entries_carry_capture_timestamps() {
        let mut history = ClipboardHistory::new();
        history.add_at("first", 1_000);
        history.add_at("second", 2_000);
        assert_eq!(history.get(0), Some("second"));
        let entries = history.search_entries("");
        assert_eq!(entries[0].text, "second");
        assert_eq!(entries[0].timestamp, 2_000);
        assert_eq!(entries[1].text, "first");
        assert_eq!(entries[1].timestamp, 1_000);
    }

    #[test]
    fn retention_prunes_entries_older_than_the_window() {
        // `add_at`/`set_retention_seconds` eagerly prune against the wall
        // clock, so the entries are placed relative to it: one well short of
        // the retention window, one far beyond it.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut history = ClipboardHistory::with_limit(10);
        history.add_at("old", now.saturating_sub(100_000));
        history.add_at("still fresh", now.saturating_sub(4_000));
        history.set_retention_seconds(Some(86_400));
        history.prune_at(now);
        assert_eq!(history.entries().collect::<Vec<_>>(), ["still fresh"]);
    }

    #[test]
    fn retention_prunes_on_access_not_only_at_set_time() {
        let mut history = ClipboardHistory::with_limit(10);
        history.add_at("old", 1_000);
        history.add_at("fresh", 1_000_000);
        history.set_retention_seconds(Some(86_400));
        history.prune_at(2_000_000_000);
        let remaining: Vec<&str> = history.entries().collect();
        assert!(remaining.is_empty(), "everything aged past the retention window");
        assert!(history.is_empty());
    }

    #[test]
    fn disabling_retention_stops_pruning() {
        let mut history = ClipboardHistory::with_limit(10);
        history.add_at("kept", 1_000);
        history.set_retention_seconds(Some(1));
        history.prune_at(1_000_500);
        assert_eq!(history.entries().count(), 0);
        history.set_retention_seconds(None);
        history.add_at("again", 1_001);
        history.prune_at(4_000_000_000);
        assert_eq!(history.entries().collect::<Vec<_>>(), ["again"]);
    }

    #[test]
    fn a_session_like_history_without_retention_never_prunes() {
        let mut history = ClipboardHistory::new();
        history.add_at("forever", 1);
        history.prune_at(4_000_000_000);
        assert_eq!(history.entries().collect::<Vec<_>>(), ["forever"]);
    }

    #[test]
    fn object_json_round_trips_with_timestamps() {
        let mut history = ClipboardHistory::new();
        history.add_at("a", 1_000);
        history.add_at("b", 2_000);
        let json = history.to_json();
        let mut loaded = ClipboardHistory::new();
        assert!(loaded.from_json(&json));
        assert_eq!(loaded.entries().collect::<Vec<_>>(), ["b", "a"]);
        let entries = loaded.search_entries("");
        assert_eq!(entries[0].timestamp, 2_000);
        assert_eq!(entries[1].timestamp, 1_000);
    }

    #[test]
    fn legacy_string_arrays_load_as_fresh_entries() {
        let mut history = ClipboardHistory::new();
        assert!(history.from_json(r#"["old copy", "newer"]"#));
        assert_eq!(
            history.entries().collect::<Vec<_>>(),
            ["old copy", "newer"],
            "legacy strings are kept, treated as freshly copied"
        );
    }

    #[test]
    fn object_entries_without_a_text_field_are_rejected() {
        let mut history = history(&["keepme"]);
        assert!(!history.from_json(r#"[{"timestamp": 1}]"#));
        assert_eq!(history.entries().collect::<Vec<_>>(), ["keepme"]);
    }
}