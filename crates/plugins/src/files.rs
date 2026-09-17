//! File-search plugin: searches the OS index via `plocate` / `locate`, or a
//! home-scoped `find` fallback when neither locate binary is present.

use crate::{has_prefix, Plugin, PluginAction, PluginResult};
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Command as StdCommand, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command as TokioCommand};
use tokio::runtime::Handle;
use tokio::time::sleep;

pub const FILE_SEARCH_DEBOUNCE_MS: u64 = 1000;
pub const FILE_SEARCH_MIN_QUERY_LEN: usize = 2;

pub const FILE_PLUGIN_ID: &str = "files";
pub const FILE_PREFIX: &str = "f:";
const KEEP_TYPING_HINT: &str = "keep typing to search files…";
const SEARCHING_HINT: &str = "Searching files…";

pub trait FileSearchBackend: Send + Sync {
    fn search(&self, query: &str, limit: usize) -> Vec<FileEntry>;
    fn spawn_search(&self, query: &str, limit: usize) -> io::Result<Child>;
    fn read_results<'a>(
        &'a self,
        child: &'a mut Child,
        query: &str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<FileEntry>>> + Send + 'a>> {
        let _ = query;
        let _ = limit;
        Box::pin(async move {
            let stdout = child.stdout.take().ok_or_else(|| io::Error::new(io::ErrorKind::Other, "child stdout unavailable"))?;
            let mut reader = BufReader::new(stdout).lines();
            let mut results = Vec::new();
            while let Some(line) = reader.next_line().await? {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(entry) = entry_from_path(line) {
                    results.push(entry);
                    if results.len() >= limit {
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
            let _ = child.wait().await;
            Ok(results)
        })
    }
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
pub struct PlocateBackend {
    command: String,
    missing_db: Arc<Mutex<bool>>,
}

impl PlocateBackend {
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
}

impl FileSearchBackend for PlocateBackend {
    fn search(&self, query: &str, limit: usize) -> Vec<FileEntry> {
        if *self.missing_db.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            return Vec::new();
        }
        let mut cmd = StdCommand::new(&self.command);
        cmd.arg("--limit").arg(limit.to_string()).arg(query);
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
        parse_lines(&output.stdout, limit)
    }

    fn spawn_search(&self, query: &str, limit: usize) -> io::Result<Child> {
        let mut cmd = TokioCommand::new(&self.command);
        cmd.arg("--limit").arg(limit.to_string()).arg(query);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.spawn()
    }

    fn read_results<'a>(
        &'a self,
        child: &'a mut Child,
        _query: &str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<FileEntry>>> + Send + 'a>> {
        Box::pin(async move {
            let stdout = child.stdout.take().ok_or_else(|| io::Error::new(io::ErrorKind::Other, "locate did not expose stdout"))?;
            let mut reader = BufReader::new(stdout).lines();
            let mut results = Vec::new();
            while let Some(line) = reader.next_line().await? {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(entry) = entry_from_path(line) {
                    results.push(entry);
                    if results.len() >= limit {
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
            let status = child.wait().await?;
            if !status.success() {
                return Ok(Vec::new());
            }
            Ok(results)
        })
    }

    fn empty_hint(&self) -> Option<String> {
        if *self.missing_db.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) {
            Some("File index is still being built; wait a moment or run updatedb.".to_owned())
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
        let mut cmd = StdCommand::new("find");
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
        parse_lines(&output.stdout, limit)
    }

    fn spawn_search(&self, query: &str, _limit: usize) -> io::Result<Child> {
        let mut cmd = TokioCommand::new("find");
        cmd.arg(&self.scope);
        if !query.is_empty() {
            cmd.arg("-iname");
            cmd.arg(format!("*{query}*"));
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.spawn()
    }

    fn read_results<'a>(
        &'a self,
        child: &'a mut Child,
        _query: &str,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<FileEntry>>> + Send + 'a>> {
        Box::pin(async move {
            let stdout = child.stdout.take().ok_or_else(|| io::Error::new(io::ErrorKind::Other, "find did not expose stdout"))?;
            let mut reader = BufReader::new(stdout).lines();
            let mut results = Vec::new();
            while let Some(line) = reader.next_line().await? {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(entry) = entry_from_path(line) {
                    results.push(entry);
                    if results.len() >= limit {
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
            let _ = child.wait().await;
            Ok(results)
        })
    }

    fn empty_hint(&self) -> Option<String> {
        self.home_hint()
    }
}

pub fn detect_file_backend(restrict_to_home: bool) -> Arc<dyn FileSearchBackend> {
    if which("plocate") {
        return Arc::new(PlocateBackend::new("plocate"));
    }
    if which("mlocate") {
        return Arc::new(PlocateBackend::new("mlocate"));
    }
    if which("locate") {
        return Arc::new(PlocateBackend::new("locate"));
    }
    Arc::new(FindBackend::new(restrict_to_home))
}

#[derive(Default)]
struct SearchState {
    query: Option<String>,
    cached_results: Vec<PluginResult>,
    searching: bool,
}

pub struct FileSearchPlugin {
    backend: Arc<dyn FileSearchBackend>,
    limit: usize,
    runtime: Option<Handle>,
    generation: Arc<AtomicU64>,
    state: Arc<Mutex<SearchState>>,
}

impl FileSearchPlugin {
    pub fn new(backend: Arc<dyn FileSearchBackend>, limit: usize, _restrict_to_home: bool) -> Self {
        Self {
            backend,
            limit,
            runtime: None,
            generation: Arc::new(AtomicU64::new(0)),
            state: Arc::new(Mutex::new(SearchState::default())),
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
        if filter.is_empty() || filter.len() < FILE_SEARCH_MIN_QUERY_LEN {
            return Vec::new();
        }

        {
            let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.query.as_deref() == Some(filter) && !state.cached_results.is_empty() {
                return state.cached_results.clone();
            }
        }

        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            state.query = Some(filter.to_owned());
            state.cached_results.clear();
            state.searching = true;
        }

        let Some(runtime) = self.runtime.clone() else {
            return Vec::new();
        };

        let backend = self.backend.clone();
        let limit = self.limit;
        let current_query = filter.to_owned();
        let generation_state = self.generation.clone();
        let state = self.state.clone();

        runtime.spawn(async move {
            sleep(Duration::from_millis(FILE_SEARCH_DEBOUNCE_MS)).await;
            if generation_state.load(Ordering::SeqCst) != generation {
                return;
            }

            let mut child = match backend.spawn_search(&current_query, limit) {
                Ok(child) => child,
                Err(_) => {
                    let mut state = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    if state.query.as_deref() == Some(&current_query) {
                        state.searching = false;
                    }
                    return;
                }
            };

            if generation_state.load(Ordering::SeqCst) != generation {
                let _ = child.kill().await;
                return;
            }

            let results = match backend.read_results(&mut child, &current_query, limit).await {
                Ok(entries) => entries,
                Err(_) => Vec::new(),
            };

            if generation_state.load(Ordering::SeqCst) != generation {
                return;
            }

            let plugin_results = results
                .into_iter()
                .map(|entry| {
                    let subtitle = entry
                        .path
                        .parent()
                        .and_then(|path| path.to_str())
                        .map(str::to_owned)
                        .unwrap_or_default();
                    PluginResult::with_subtitle(entry.name.clone(), subtitle)
                        .with_icon(entry.icon_name().to_owned())
                        .with_tag("file")
                        .with_action(PluginAction::Open {
                            path: entry.path.to_string_lossy().to_string(),
                        })
                })
                .collect::<Vec<_>>();

            let mut state = state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.query.as_deref() == Some(&current_query) {
                state.cached_results = plugin_results;
                state.searching = false;
            }
        });

        Vec::new()
    }

    fn empty_hint(&self, query: &str) -> Option<String> {
        let Some(filter) = self.trimmed_query(query) else {
            return None;
        };
        if filter.is_empty() {
            return None;
        }
        if filter.len() < FILE_SEARCH_MIN_QUERY_LEN {
            return Some(KEEP_TYPING_HINT.to_owned());
        }

        let state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.query.as_deref() == Some(filter) && state.searching {
            return Some(SEARCHING_HINT.to_owned());
        }
        if let Some(hint) = self.backend.empty_hint() {
            return Some(hint);
        }
        if state.query.as_deref() == Some(filter) && state.cached_results.is_empty() {
            return Some("No files matched this query.".to_owned());
        }
        None
    }
}

fn parse_lines(stdout: &[u8], limit: usize) -> Vec<FileEntry> {
    let stdout = String::from_utf8_lossy(stdout);
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| entry_from_path(line.trim()))
        .take(limit)
        .collect()
}

fn entry_from_path(value: &str) -> Option<FileEntry> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return None;
    }
    let name = path.file_name()?.to_string_lossy().to_string();
    let is_dir = path.is_dir();
    Some(FileEntry {
        path: path.clone(),
        name,
        is_dir,
    })
}

fn which(program: &str) -> bool {
    let output = StdCommand::new("sh")
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

        fn spawn_search(&self, _query: &str, _limit: usize) -> io::Result<Child> {
            unimplemented!("fake backend does not spawn a real subprocess")
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

            fn spawn_search(&self, _query: &str, _limit: usize) -> io::Result<Child> {
                unimplemented!("panic backend is not used for async subprocesses")
            }
        }

        let plugin = FileSearchPlugin::new(Arc::new(PanicBackend), 10, true);
        let results = plugin.query("f:report");
        assert!(results.is_empty(), "query should defer backend work instead of blocking");
    }

    #[test]
    fn short_queries_show_a_keep_typing_hint() {
        let plugin = FileSearchPlugin::new(Arc::new(FakeBackend), 10, true);
        assert!(plugin.matches("f:a"));
        assert_eq!(plugin.empty_hint("f:a"), Some("keep typing to search files…".to_owned()));
    }

    #[test]
    fn locate_backend_uses_file_entries_and_limits() {
        let backend = PlocateBackend::new("/bin/true");
        let rows = backend.search("hello", 2);
        assert!(rows.is_empty());
    }
}
