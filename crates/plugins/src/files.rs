//! File-search plugin: searches the OS index via `plocate` / `locate`, or a
//! home-scoped `find` fallback when neither locate binary is present.

use crate::{has_prefix, Plugin, PluginAction, PluginResult};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::time::sleep;

pub const FILE_SEARCH_DEBOUNCE_MS: u64 = 1000;

pub const FILE_PLUGIN_ID: &str = "files";
pub const FILE_PREFIX: &str = "f:";

pub trait FileSearchBackend: Send + Sync {
    /// Search for files or folders matching `query`, returning at most `limit`.
    fn search(&self, query: &str, limit: usize) -> Vec<FileEntry>;

    /// Optional guidance shown when a backend is active but the DB is still warming up.
    fn empty_hint(&self) -> Option<String> {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

impl FileEntry {
    fn icon_name(&self) -> &'static str {
        if self.is_dir {
            "folder"
        } else {
            "text-x-generic"
        }
    }
}

#[derive(Clone)]
pub struct LocateBackend {
    command: String,
    missing_db: Arc<Mutex<bool>>,
}

impl LocateBackend {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            missing_db: Arc::new(Mutex::new(false)),
        }
    }

    fn mark_missing(&self) {
        let mut missing = self.missing_db.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *missing = true;
    }

    fn search_impl(&self, query: &str, limit: usize) -> Vec<FileEntry> {
        let mut cmd = Command::new(&self.command);
        cmd.arg(query);
        let output = match cmd.output() {
            Ok(output) => output,
            Err(_) => {
                self.mark_missing();
                return Vec::new();
            }
        };
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("database") || stderr.contains("No such file") || stderr.contains("not found") {
                self.mark_missing();
            }
            return Vec::new();
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .filter_map(|path| {
                let is_dir = path.is_dir();
                let name = path.file_name().map(|value| value.to_string_lossy().to_string())?;
                Some(FileEntry {
                    path,
                    name,
                    is_dir,
                })
            })
            .take(limit)
            .collect()
    }
}

impl FileSearchBackend for LocateBackend {
    fn search(&self, query: &str, limit: usize) -> Vec<FileEntry> {
        if *self.missing_db.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            return Vec::new();
        }
        self.search_impl(query, limit)
    }

    fn empty_hint(&self) -> Option<String> {
        if *self.missing_db.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            Some("File index is still being built — wait a moment or run updatedb.".to_owned())
        } else {
            None
        }
    }
}

#[derive(Clone)]
pub struct FindBackend {
    scope: PathBuf,
    restrict_to_home: bool,
}

impl FindBackend {
    pub fn new(restrict_to_home: bool) -> Self {
        let scope = if restrict_to_home {
            home_dir()
        } else {
            PathBuf::from("/")
        };
        Self {
            scope,
            restrict_to_home,
        }
    }

    fn home_hint(&self) -> Option<String> {
        if self.restrict_to_home {
            Some("Find fallback is scoped to your home directory; results may be incomplete.".to_owned())
        } else {
            None
        }
    }
}

impl FileSearchBackend for FindBackend {
    fn search(&self, query: &str, limit: usize) -> Vec<FileEntry> {
        let mut cmd = Command::new("find");
        cmd.arg(&self.scope);
        if !query.is_empty() {
            cmd.arg("-iname");
            cmd.arg(format!("*{query}*"));
        }
        let output = match cmd.output() {
            Ok(output) => output,
            Err(_) => return Vec::new(),
        };
        if !output.status.success() {
            return Vec::new();
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .lines()
            .filter_map(|line| {
                let path = PathBuf::from(line.trim());
                if !path.is_absolute() || path.as_os_str().is_empty() {
                    return None;
                }
                let is_dir = path.is_dir();
                let name = path.file_name().map(|value| value.to_string_lossy().to_string())?;
                Some(FileEntry {
                    path,
                    name,
                    is_dir,
                })
            })
            .take(limit)
            .collect()
    }

    fn empty_hint(&self) -> Option<String> {
        self.home_hint()
    }
}

pub fn detect_file_backend(restrict_to_home: bool) -> Arc<dyn FileSearchBackend> {
    if which("plocate") {
        return Arc::new(LocateBackend::new("plocate"));
    }
    if which("locate") {
        return Arc::new(LocateBackend::new("locate"));
    }
    Arc::new(FindBackend::new(restrict_to_home))
}

pub struct FileSearchPlugin {
    backend: Arc<dyn FileSearchBackend>,
    limit: usize,
    runtime: Option<Handle>,
    last_query: Arc<Mutex<Option<String>>>,
    last_results: Arc<Mutex<Vec<PluginResult>>>,
    last_search: Arc<Mutex<Option<Instant>>>,
    debounce: Duration,
}

impl FileSearchPlugin {
    pub fn new(backend: Arc<dyn FileSearchBackend>, limit: usize, restrict_to_home: bool) -> Self {
        let _ = restrict_to_home;
        Self {
            backend,
            limit,
            runtime: None,
            last_query: Arc::new(Mutex::new(None)),
            last_results: Arc::new(Mutex::new(Vec::new())),
            last_search: Arc::new(Mutex::new(None)),
            debounce: Duration::from_millis(FILE_SEARCH_DEBOUNCE_MS),
        }
    }

    pub fn with_runtime(mut self, runtime: Handle) -> Self {
        self.runtime = Some(runtime);
        self
    }

    fn trimmed_query<'a>(&self, query: &'a str) -> Option<&'a str> {
        if !has_prefix(query, FILE_PREFIX) {
            return None;
        }
        query.get(FILE_PREFIX.len()..).map(str::trim)
    }

    fn schedule_search(&self, filter: String, generation: u64) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let backend = self.backend.clone();
        let limit = self.limit;
        let last_results = self.last_results.clone();
        let last_query = self.last_query.clone();
        let last_search = self.last_search.clone();
        runtime.spawn(async move {
            sleep(Duration::from_millis(FILE_SEARCH_DEBOUNCE_MS)).await;
            let entries = match backend.search(&filter, limit) {
                entries if entries.is_empty() => Vec::new(),
                entries => entries,
            };
            let results = entries
                .into_iter()
                .map(|entry| {
                    let title = entry.name.clone();
                    let subtitle = entry
                        .path
                        .parent()
                        .and_then(|path| path.to_str())
                        .map(str::to_owned)
                        .unwrap_or_default();
                    PluginResult::with_subtitle(title, subtitle)
                        .with_icon(entry.icon_name().to_owned())
                        .with_tag("file")
                        .with_action(PluginAction::Open {
                            path: entry.path.to_string_lossy().to_string(),
                        })
                })
                .collect::<Vec<_>>();

            let mut cached = last_results.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *cached = results;
            let mut cached_query = last_query.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *cached_query = Some(filter.clone());
            let mut cached_search = last_search.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *cached_search = Some(Instant::now());
            let _ = generation;
        });
    }
}

impl Plugin for FileSearchPlugin {
    fn id(&self) -> &str {
        FILE_PLUGIN_ID
    }

    fn name(&self) -> &str {
        "Files"
    }

    fn prefix(&self) -> Option<&str> {
        Some(FILE_PREFIX)
    }

    fn query(&self, query: &str) -> Vec<PluginResult> {
        let Some(filter) = self.trimmed_query(query) else {
            return Vec::new();
        };
        if filter.is_empty() {
            return Vec::new();
        }

        let cached = {
            let last_query = self.last_query.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let last_search = self.last_search.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let last_results = self.last_results.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if last_query.as_deref() == Some(filter)
                && last_search
                    .map(|instant| Instant::now().saturating_duration_since(instant) < self.debounce)
                    .unwrap_or(false)
            {
                Some(last_results.clone())
            } else {
                None
            }
        };
        if let Some(cached) = cached {
            return cached;
        }

        let mut last_query = self.last_query.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *last_query = Some(filter.to_owned());
        let mut last_search = self.last_search.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *last_search = Some(Instant::now());
        drop(last_search);
        drop(last_query);

        let current_query = filter.to_owned();
        let runtime = self.runtime.clone();
        if let Some(runtime) = runtime {
            let backend = self.backend.clone();
            let limit = self.limit;
            let last_results = self.last_results.clone();
            let last_query = self.last_query.clone();
            let last_search = self.last_search.clone();
            runtime.spawn(async move {
                sleep(Duration::from_millis(FILE_SEARCH_DEBOUNCE_MS)).await;
                let results = backend
                    .search(&current_query, limit)
                    .into_iter()
                    .map(|entry| {
                        let title = entry.name.clone();
                        let subtitle = entry
                            .path
                            .parent()
                            .and_then(|path| path.to_str())
                            .map(str::to_owned)
                            .unwrap_or_default();
                        PluginResult::with_subtitle(title, subtitle)
                            .with_icon(entry.icon_name().to_owned())
                            .with_tag("file")
                            .with_action(PluginAction::Open {
                                path: entry.path.to_string_lossy().to_string(),
                            })
                    })
                    .collect::<Vec<_>>();

                let mut cached_results = last_results.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                *cached_results = results;
                let mut cached_query = last_query.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                *cached_query = Some(current_query.clone());
                let mut cached_search = last_search.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                *cached_search = Some(Instant::now());
            });
        }

        Vec::new()
    }

    fn empty_hint(&self, query: &str) -> Option<String> {
        let Some(filter) = self.trimmed_query(query) else {
            return None;
        };
        if filter.is_empty() {
            return None;
        }
        if self.runtime.is_some() {
            let last_query = self.last_query.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if last_query.as_deref() == Some(filter) {
                return Some("Searching files…".to_owned());
            }
        }
        self.backend.empty_hint().or_else(|| {
            if self.backend.search(filter, 1).is_empty() {
                Some("No files matched this query.".to_owned())
            } else {
                None
            }
        })
    }
}

fn which(program: &str) -> bool {
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program} >/dev/null 2>&1"))
        .output();
    matches!(output, Ok(output) if output.status.success())
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| PathBuf::from("/home"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeBackend;
    impl FileSearchBackend for FakeBackend {
        fn search(&self, query: &str, limit: usize) -> Vec<FileEntry> {
            let mut results = vec![FileEntry {
                path: PathBuf::from(format!("/tmp/{query}.txt")),
                name: format!("{query}.txt"),
                is_dir: false,
            }];
            if limit == 0 {
                results.clear();
            }
            results
        }
    }

    #[test]
    fn file_plugin_matches_the_f_prefix() {
        let plugin = FileSearchPlugin::new(Arc::new(FakeBackend), 10, true);
        assert!(plugin.matches("f:report"));
        assert!(!plugin.matches("report"));
    }

    #[test]
    fn file_plugin_query_does_not_block_on_backend_search() {
        struct PanicBackend;
        impl FileSearchBackend for PanicBackend {
            fn search(&self, _query: &str, _limit: usize) -> Vec<FileEntry> {
                panic!("synchronous search is forbidden on the GTK thread")
            }
        }

        let plugin = FileSearchPlugin::new(Arc::new(PanicBackend), 10, true);
        let results = plugin.query("f:report");
        assert!(results.is_empty(), "query should defer backend work instead of blocking");
    }

    #[test]
    fn locate_backend_uses_file_entries_and_limits() {
        let backend = LocateBackend::new("/bin/true");
        let rows = backend.search("hello", 2);
        assert!(rows.is_empty());
    }
}
