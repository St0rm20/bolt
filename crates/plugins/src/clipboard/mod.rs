//! Clipboard-history plugin: a background [`monitor`] fills a shared
//! [`history`], and the [`plugin`] serves it through the `clip:` prefix.
//!
//! The data flow follows the architecture the task specifies:
//!
//! ```text
//! Clipboard monitor (background thread)
//!        |
//!        v
//! ClipboardHistory  (Arc<Mutex<_>>, bounded, persisted)
//!        |
//!        v
//! ClipboardPlugin   (read-only queries during `clip:...`)
//!        |
//!        v
//! PluginRegistry --> GTK launcher (rows + PluginAction::Copy)
//! ```
//!
//! Monitoring and querying never share a lock for long: the monitor folds new
//! text into the history and persists it on real changes; the plugin only
//! scans the history on `clip:` keystrokes. Both are GTK-free and tested with
//! in-memory fakes — the only piece that talks to a display is
//! [`ArboardClipboardSource`], wrapped in a trait for dependency injection.

pub mod history;
pub mod monitor;
pub mod plugin;

pub use history::{
    default_data_dir, default_history_path, load_into, ClipboardHistory, LoadOutcome,
};
pub use monitor::{ArboardClipboardSource, ClipboardError, ClipboardMonitor, ClipboardSource};
pub use plugin::ClipboardPlugin;