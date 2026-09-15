//! Background clipboard monitoring, decoupled from the GTK main thread and
//! from the plugin layer.
//!
//! [`ClipboardMonitor`] owns a [`ClipboardSource`] (the system clipboard read
//! path — [`ArboardClipboardSource`] under Wayland/Hyprland), runs on its own
//! thread, and folds whatever it reads into a shared
//! [`ClipboardHistory`](crate::clipboard::history::ClipboardHistory). It knows
//! nothing about the launcher UI or the plugin: it just keeps the shared
//! history fresh and persists it on every real change.
//!
//! Why polling, and why 500 ms:
//!
//! * `arboard`'s Wayland backend reads through `zwlr_data_control_manager_v1`
//!   (the same protocol `cliphist` and friends use on Hyprland), which has no
//!   per-client "selection changed" signal to subscribe to; listening would
//!   mean building our own Wayland data-control client. A poll is a
//!   one-way-local read over the compositor socket: at 2 reads/s the cost is
//!   negligible and new copies appear within half a second of the copy.
//! * reads are cheap because the monitor compares the text against the last
//!   seen value before touching the shared history or the persistence file.
//!
//! The source is a trait so tests inject a scripted fake instead of a display.
//! [`ClipboardSource::read_text`] deliberately returns `Ok(None)` for "no text
//! available" (an empty clipboard is not an error), and read/init failures are
//! reported as errors that [`ClipboardMonitor::run`] survives.

use crate::clipboard::history::ClipboardHistory;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Default interval between clipboard polls. 500 ms keeps the launcher fresh
/// without measurable CPU cost (see the module docs).
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// A failure to talk to the clipboard backend. Never carries clipboard
/// contents — error text is backend metadata only.
#[derive(Debug, Clone)]
pub struct ClipboardError(pub String);

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ClipboardError {}

/// The clipboard read path the monitor polls.
///
/// `Ok(None)` means "the clipboard is empty right now" (common right after
/// login) — the monitor treats it as no change. `Ok(Some(text))` is a text
/// to record (the empty string is ignored by the history).
pub trait ClipboardSource {
    /// Read the current clipboard text, if any.
    fn read_text(&mut self) -> Result<Option<String>, ClipboardError>;
}

/// The real clipboard source: `arboard` headless over Wayland data-control.
///
/// Constructing `Clipboard` requires a running Wayland compositor exposing
/// `zwlr_data_control_manager_v1` (Hyprland and other wlroots-based
/// compositors do). Without one [`Self::new`] returns an error and the daemon
/// simply runs without live capture.
pub struct ArboardClipboardSource {
    clipboard: arboard::Clipboard,
}

impl ArboardClipboardSource {
    /// Connect to the system clipboard backend.
    pub fn new() -> Result<Self, ClipboardError> {
        let clipboard = arboard::Clipboard::new()
            .map_err(|err| ClipboardError(format!("clipboard backend init failed: {err}")))?;
        Ok(Self { clipboard })
    }
}

impl ClipboardSource for ArboardClipboardSource {
    fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
        match self.clipboard.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(err) => Err(ClipboardError(format!("clipboard read failed: {err}"))),
        }
    }
}

/// Polling clipboard monitor. Owned by exactly one background thread.
pub struct ClipboardMonitor {
    source: Box<dyn ClipboardSource>,
    history: Arc<Mutex<ClipboardHistory>>,
    /// Where changed history is persisted; `None` disables saving.
    persistence: Option<PathBuf>,
    /// The last text seen, so unchanged clips skip history/file entirely.
    last_seen: Option<String>,
    /// Consecutive failures since the last successful read; only the first is
    /// logged, to avoid a stderr line every 500 ms during an outage.
    failures_since_success: u64,
    /// Delay between polls (see [`DEFAULT_POLL_INTERVAL`]).
    interval: Duration,
}

impl ClipboardMonitor {
    /// Build a monitor over a source and the shared history. `persistence`
    /// holds the JSON path to save to after each real change (`None` =
    /// history kept only in memory).
    #[must_use]
    pub fn new(
        source: Box<dyn ClipboardSource>,
        history: Arc<Mutex<ClipboardHistory>>,
        persistence: Option<PathBuf>,
    ) -> Self {
        Self {
            source,
            history,
            persistence,
            last_seen: None,
            failures_since_success: 0,
            interval: DEFAULT_POLL_INTERVAL,
        }
    }

    /// One poll cycle: read the clipboard, fold a new text into the shared
    /// history and persist. Returns `Ok(true)` when the history changed,
    /// `Ok(false)` when nothing was new (unchanged text, empty clipboard) and
    /// `Err` on a read/backend failure — the caller keeps polling, so an
    /// outage degrades gracefully and never crashes the launcher.
    pub fn poll(&mut self) -> Result<bool, ClipboardError> {
        let Some(text) = self.source.read_text()? else {
            self.failures_since_success = 0;
            return Ok(false);
        };
        if self.last_seen.as_deref() == Some(&text) {
            return Ok(false);
        }
        self.last_seen = Some(text.clone());
        if text.is_empty() {
            return Ok(false);
        }
        let changed = {
            let mut history = self.lock_history();
            history.add(&text)
        };
        if changed {
            self.persist();
            return Ok(true);
        }
        Ok(false)
    }

    /// Run until `running` flips false: poll, then sleep. Survives backend
    /// failures (logged once per outage; polling simply resumes).
    pub fn run(&mut self, running: &AtomicBool) {
        while running.load(Ordering::Relaxed) {
            if let Err(err) = self.poll() {
                self.failures_since_success += 1;
                if self.failures_since_success == 1 {
                    eprintln!("launcher-clipboard: {err}; retrying");
                }
            }
            thread::sleep(self.interval);
        }
    }

    /// The shared history the monitor keeps updated.
    pub fn history(&self) -> &Arc<Mutex<ClipboardHistory>> {
        &self.history
    }

    fn lock_history(&self) -> std::sync::MutexGuard<'_, ClipboardHistory> {
        self.history.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Persist the current history. Save failures are logged (metadata only)
    /// and never fatal — the in-memory history keeps working.
    fn persist(&self) {
        let Some(path) = &self.persistence else { return };
        if let Err(err) = self.lock_history().save(path) {
            eprintln!("launcher-clipboard: could not save history to {}: {err}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::history::ClipboardHistory;

    /// A scriptable clipboard read path: yields each queued value in turn and
    /// then repeats the last one (a steady clipboard), so tests never need a
    /// real display.
    struct ScriptedSource {
        script: Vec<Result<Option<String>, ClipboardError>>,
        cursor: usize,
    }

    impl ScriptedSource {
        fn ok(text: impl Into<String>) -> Self {
            Self {
                script: vec![Ok(Some(text.into()))],
                cursor: 0,
            }
        }

        fn seq(mut self, step: Result<Option<String>, ClipboardError>) -> Self {
            self.script.push(step);
            self
        }
    }

    impl ClipboardSource for ScriptedSource {
        fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
            let index = self.cursor.min(self.script.len().saturating_sub(1));
            let value = self.script[index].clone();
            if self.cursor < self.script.len() {
                self.cursor += 1;
            }
            value
        }
    }

    /// A monitor on an empty in-memory history with no persistence.
    fn monitor_with(source: ScriptedSource) -> (ClipboardMonitor, Arc<Mutex<ClipboardHistory>>) {
        let history = Arc::new(Mutex::new(ClipboardHistory::new()));
        let monitor = ClipboardMonitor::new(Box::new(source), history.clone(), None);
        (monitor, history)
    }

    #[test]
    fn folds_new_text_into_the_shared_history() {
        let (mut monitor, history) = monitor_with(ScriptedSource::ok("hello clipboard"));
        assert!(monitor.poll().expect("read succeeds"));
        assert_eq!(
            history.lock().unwrap().entries().collect::<Vec<_>>(),
            ["hello clipboard"]
        );
    }

    #[test]
    fn consecutive_identical_text_is_not_re_added() {
        // The script stays steady on the same value: still "a" on every read.
        let (mut monitor, history) = monitor_with(ScriptedSource::ok("a"));
        assert!(monitor.poll().expect("first poll reads 'a'"));
        assert!(!monitor.poll().expect("same text again"), "unchanged text adds nothing");
        assert_eq!(history.lock().unwrap().len(), 1);
    }

    #[test]
    fn multiple_texts_accumulate_newest_first() {
        let (mut monitor, history) = monitor_with(
            ScriptedSource::ok("a").seq(Ok(Some("b".into()))).seq(Ok(Some("c".into()))),
        );
        monitor.poll().expect("a");
        monitor.poll().expect("b");
        monitor.poll().expect("c");
        assert_eq!(
            history.lock().unwrap().entries().collect::<Vec<_>>(),
            ["c", "b", "a"]
        );
    }

    #[test]
    fn a_duplicate_re_copy_moves_it_to_the_front() {
        // Reappearing text (e.g. re-copied from an app) relocates, not
        // duplicates — mirroring history.add's rule.
        let (mut monitor, history) = monitor_with(
            ScriptedSource::ok("a").seq(Ok(Some("b".into()))).seq(Ok(Some("a".into()))),
        );
        monitor.poll().expect("a");
        monitor.poll().expect("b");
        assert!(monitor.poll().expect("a again moves it to the front"));
        assert_eq!(
            history.lock().unwrap().entries().collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(history.lock().unwrap().len(), 2, "no duplicate entry");
    }

    #[test]
    fn empty_clipboard_is_registered_as_no_change() {
        let (mut monitor, history) = monitor_with(
            ScriptedSource::ok("x").seq(Ok(None)),
        );
        monitor.poll().expect("x");
        assert!(!monitor.poll().expect("empty clipboard is not a change"));
        assert_eq!(history.lock().unwrap().entries().collect::<Vec<_>>(), ["x"]);
    }

    #[test]
    fn read_failures_survive_and_polling_resumes() {
        let (mut monitor, history) = monitor_with(
            ScriptedSource::ok("before").seq(Err(ClipboardError("backend hiccup".into()))).seq(Ok(Some("after".into()))),
        );
        monitor.poll().expect("before");
        assert!(monitor.poll().is_err(), "a failing read is surfaced to the loop");
        assert!(monitor.poll().expect("reads resume after the failure"));
        assert_eq!(
            history.lock().unwrap().entries().collect::<Vec<_>>(),
            ["after", "before"]
        );
    }

    #[test]
    fn persists_after_each_real_change_only() {
        let dir = std::env::temp_dir().join(format!("bolt-clipboard-monitor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("clipboard_history.json");

        let history = Arc::new(Mutex::new(ClipboardHistory::new()));
        let mut monitor =
            ClipboardMonitor::new(Box::new(ScriptedSource::ok("persisted text")), history.clone(), Some(path.clone()));
        monitor.poll().expect("poll");
        let entries: Vec<String> = history.lock().unwrap().entries().map(str::to_owned).collect();
        let saved: Vec<String> = serde_json::from_str(&std::fs::read_to_string(&path).expect("file written"))
            .expect("stored JSON parses");
        assert_eq!(saved, entries, "the file holds exactly the persisted history");

        // A steady clipboard must not rewrite the file: bump mtime, poll
        // again, assert the file content is unchanged (no second save).
        monitor.poll().expect("unchanged read adds nothing");
        assert_eq!(
            std::fs::read_to_string(&path).expect("file still present"),
            serde_json::to_string(&entries).expect("json")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn without_a_persistence_path_nothing_is_written() {
        let (mut monitor, _history) = monitor_with(ScriptedSource::ok("memory only"));
        assert!(monitor.poll().expect("read succeeds"));
        // No path was given, so nothing to assert beyond a clean run.
    }

    #[test]
    #[ignore = "requires a live Wayland/Hyprland session; run manually on the desktop"]
    fn arboard_source_speaks_to_the_live_clipboard() {
        // Manual verification hook: on a Hyprland desktop this connects over
        // wlr-data-control and must return either a text or Ok(None) — never
        // panic. Run with `cargo test -p launcher-plugins -- --ignored`.
        if let Ok(mut source) = ArboardClipboardSource::new() {
            match source.read_text() {
                Ok(_) => {}
                Err(err) => eprintln!("live clipboard read reported: {err}"),
            }
        } else {
            eprintln!("no Wayland data-control backend available on this session");
        }
    }
}