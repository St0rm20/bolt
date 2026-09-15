//! Application indexing: discover installed applications from freedesktop
//! `.desktop` entries.
//!
//! The indexer is independent of GTK, tokio and the daemon. It scans the
//! configured application directories, parses each `.desktop` file, drops
//! entries that must not be shown (`Type` other than `Application`,
//! `NoDisplay=true`, missing required fields) and returns a deterministic
//! [`Vec<AppEntry>`].
//!
//! The caller (daemon or UI) owns the resulting index and decides when to
//! rebuild it; nothing here keeps global mutable state. Calling
//! [`AppIndexer::build_index`] again simply produces a fresh report.
//!
//! Directory order controls duplicate precedence: when two directories
//! contain a desktop entry with the same id, the entry from the *earliest*
//! directory in the vector wins. [`AppIndexer::standard`] therefore puts the
//! user directory before the system one, so user-specific entries override
//! system-wide entries with the same id.
//!
//! Missing or unreadable directories are tolerated: they are reported in
//! [`IndexReport::skipped`] and the remaining directories are still scanned.
//! One malformed file never aborts the build.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

mod parser;

/// A single indexed application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// Stable desktop entry id: the file name without the `.desktop`
    /// extension (e.g. `org.gnome.Nautilus`). Stable enough to distinguish
    /// applications with similar display names.
    pub id: String,
    /// Human-readable application name (`Name`).
    pub name: String,
    /// Command line to launch the application (`Exec`), kept verbatim
    /// including `%` placeholders and quoting. Never executed by the indexer.
    pub exec: String,
    /// Icon name (`Icon`), when the entry specifies one.
    pub icon: Option<String>,
}

/// A file or directory that was skipped, together with the reason.
#[derive(Debug)]
pub struct Skipped {
    /// Path of the skipped directory or file.
    pub path: PathBuf,
    /// Why it was skipped.
    pub error: IndexerError,
}

/// Result of one index build: the valid entries plus everything that was not
/// indexed.
#[derive(Debug, Default)]
pub struct IndexReport {
    /// Indexed applications, sorted by [`AppEntry::id`].
    pub entries: Vec<AppEntry>,
    /// Directories and files that were skipped, with reasons.
    pub skipped: Vec<Skipped>,
}

impl IndexReport {
    /// Whether every discoverable file was indexed successfully.
    pub fn is_complete(&self) -> bool {
        self.skipped.is_empty()
    }
}

/// Errors gathered while building the index.
///
/// Individual problems never abort a build; they are collected in
/// [`IndexReport::skipped`] so the remaining files can still be indexed.
#[derive(Debug)]
pub enum IndexerError {
    /// The application directory could not be opened or listed.
    DirectoryRead {
        path: PathBuf,
        source: std::io::Error,
    },
    /// A `.desktop` file could not be read: permissions, broken symlink,
    /// invalid UTF-8, and so on.
    FileRead {
        path: PathBuf,
        source: std::io::Error,
    },
    /// The file is not a well-formed desktop entry.
    Parse {
        path: PathBuf,
        detail: String,
    },
    /// The file parsed successfully but is not indexable as an application.
    InvalidEntry {
        path: PathBuf,
        reason: &'static str,
    },
}

impl fmt::Display for IndexerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexerError::DirectoryRead { path, source } => {
                write!(f, "cannot read application directory {}: {source}", path.display())
            }
            IndexerError::FileRead { path, source } => {
                write!(f, "cannot read {}: {source}", path.display())
            }
            IndexerError::Parse { path, detail } => {
                write!(f, "cannot parse {}: {detail}", path.display())
            }
            IndexerError::InvalidEntry { path, reason } => {
                write!(f, "{} is not indexable: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for IndexerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            IndexerError::DirectoryRead { source, .. } | IndexerError::FileRead { source, .. } => {
                Some(source)
            }
            IndexerError::Parse { .. } | IndexerError::InvalidEntry { .. } => None,
        }
    }
}

/// The conventional application directories: the user directory first, then
/// `/usr/share/applications`.
pub fn standard_directories() -> Vec<PathBuf> {
    let mut directories = Vec::with_capacity(4);
    if let Some(user) = user_applications_dir() {
        directories.push(user);
    }
    // Flatpak apps export their .desktop files through the user/system export
    // dirs; they come after the user dir and before the system apps dir.
    directories.extend(flatpak_directories());
    directories.push(PathBuf::from("/usr/share/applications"));
    directories
}

/// The Flatpak `.desktop` export directories that exist on this machine,
/// user first: `$XDG_DATA_HOME/flatpak/exports/share/applications` (or
/// `$HOME/.local/share/flatpak/exports/share/applications`) and then
/// `/var/lib/flatpak/exports/share/applications`.
///
/// Only directories that actually exist are returned, so a machine without
/// Flatpak simply yields an empty list instead of reporting spurious skips.
#[must_use]
pub fn flatpak_directories() -> Vec<PathBuf> {
    let mut directories = Vec::with_capacity(2);
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        if !data_home.is_empty() {
            let candidate = PathBuf::from(data_home).join("flatpak/exports/share/applications");
            if candidate.is_dir() {
                directories.push(candidate);
            }
            return directories;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/share/flatpak/exports/share/applications");
        if candidate.is_dir() {
            directories.push(candidate);
        }
    }
    let system = PathBuf::from("/var/lib/flatpak/exports/share/applications");
    if system.is_dir() {
        directories.push(system);
    }
    directories
}

/// Discovers installed applications from an ordered set of directories.
///
/// The index is always rebuilt from scratch; an [`AppIndexer`] is cheap to
/// keep around and can be reused indefinitely.
#[derive(Debug, Clone)]
pub struct AppIndexer {
    directories: Vec<PathBuf>,
}

impl AppIndexer {
    /// Index the given directories. Earlier directories take precedence over
    /// later ones when desktop entry ids collide.
    pub fn new(directories: Vec<PathBuf>) -> Self {
        Self { directories }
    }

    /// Index the standard system and user application directories. The user
    /// directory comes first so user-specific entries override system ones.
    pub fn standard() -> Self {
        Self::new(standard_directories())
    }

    /// The directories this indexer scans, in precedence order.
    pub fn directories(&self) -> &[PathBuf] {
        &self.directories
    }

    /// Build a fresh index from all configured directories.
    ///
    /// Missing directories, unreadable files and invalid entries are recorded
    /// in [`IndexReport::skipped`]; the build itself never fails.
    pub fn build_index(&self) -> IndexReport {
        let mut by_id: HashMap<String, AppEntry> = HashMap::new();
        let mut skipped: Vec<Skipped> = Vec::new();

        for directory in &self.directories {
            scan_directory(directory, &mut by_id, &mut skipped);
        }

        let mut entries: Vec<AppEntry> = by_id.into_values().collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));

        IndexReport { entries, skipped }
    }
}

/// Resolve the user's application directory: `$XDG_DATA_HOME/applications`
/// or `$HOME/.local/share/applications`. Returns `None` when no home
/// directory is available.
fn user_applications_dir() -> Option<PathBuf> {
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME") {
        if !data_home.is_empty() {
            return Some(PathBuf::from(data_home).join("applications"));
        }
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/share/applications"))
}

/// Scan one directory for `.desktop` files. Subdirectories are never
/// descended into; missing or unreadable directories and files are recorded
/// in `skipped` and do not stop the scan.
fn scan_directory(
    directory: &Path,
    by_id: &mut HashMap<String, AppEntry>,
    skipped: &mut Vec<Skipped>,
) {
    let read = match std::fs::read_dir(directory) {
        Ok(read) => read,
        Err(source) => {
            skipped.push(Skipped {
                path: directory.to_path_buf(),
                error: IndexerError::DirectoryRead {
                    path: directory.to_path_buf(),
                    source,
                },
            });
            return;
        }
    };

    for item in read {
        let item = match item {
            Ok(item) => item,
            Err(source) => {
                skipped.push(Skipped {
                    path: directory.to_path_buf(),
                    error: IndexerError::DirectoryRead {
                        path: directory.to_path_buf(),
                        source,
                    },
                });
                continue;
            }
        };

        let path = item.path();

        // Only files with the `.desktop` extension are relevant.
        if path.extension().and_then(|ext| ext.to_str()) != Some("desktop") {
            continue;
        }

        // Never recurse into subdirectories. Everything else (regular files,
        // symlinks — including broken ones) is attempted below; read errors
        // for broken symlinks are reported as `FileRead` skips.
        let file_type = match item.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            continue;
        }

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(source) => {
                skipped.push(Skipped {
                    path: path.clone(),
                    error: IndexerError::FileRead { path, source },
                });
                continue;
            }
        };

        let parsed = match parser::parse(&contents) {
            Ok(parsed) => parsed,
            Err(parse_error) => {
                let error = IndexerError::Parse {
                    path: path.clone(),
                    detail: parse_error.to_string(),
                };
                skipped.push(Skipped { path, error });
                continue;
            }
        };

        let id = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();

        match classify(parsed, &id, &path) {
            Ok(entry) => {
                // Keep the first occurrence: earlier directories in `new`
                // take precedence over later ones.
                by_id.entry(entry.id.clone()).or_insert(entry);
            }
            Err(error) => {
                skipped.push(Skipped { path, error });
            }
        }
    }
}

/// Turn a parsed entry into an [`AppEntry`], or describe why it is not
/// indexable.
fn classify(parsed: parser::ParsedEntry, id: &str, path: &Path) -> Result<AppEntry, IndexerError> {
    if id.is_empty() {
        return Err(IndexerError::InvalidEntry {
            path: path.to_path_buf(),
            reason: "could not derive a desktop entry id from the file name",
        });
    }
    if parsed.entry_type.as_deref() != Some("Application") {
        return Err(IndexerError::InvalidEntry {
            path: path.to_path_buf(),
            reason: "Type is not 'Application'",
        });
    }
    if parsed.no_display {
        return Err(IndexerError::InvalidEntry {
            path: path.to_path_buf(),
            reason: "NoDisplay=true",
        });
    }
    let name = match trimmed_nonempty(parsed.name) {
        Some(name) => name,
        None => {
            return Err(IndexerError::InvalidEntry {
                path: path.to_path_buf(),
                reason: "missing required field 'Name'",
            });
        }
    };
    let exec = match trimmed_nonempty(parsed.exec) {
        Some(exec) => exec,
        None => {
            return Err(IndexerError::InvalidEntry {
                path: path.to_path_buf(),
                reason: "missing required field 'Exec'",
            });
        }
    };
    let icon = parsed.icon.filter(|icon| !icon.trim().is_empty());

    Ok(AppEntry {
        id: id.to_owned(),
        name,
        exec,
        icon,
    })
}

fn trimmed_nonempty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim();
        if value.is_empty() {
            None
        } else {
            Some(value.to_owned())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("launcher-index-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn valid_entry() -> &'static str {
        "[Desktop Entry]\nType=Application\nName=Terminal\nExec=kgx %U\nIcon=utilities-terminal\n"
    }

    fn ids(report: &IndexReport) -> Vec<&str> {
        report.entries.iter().map(|entry| entry.id.as_str()).collect()
    }

    #[test]
    fn indexes_a_valid_entry() {
        let dir = temp_dir("valid");
        std::fs::write(dir.join("org.gnome.Terminal.desktop"), valid_entry()).unwrap();

        let report = AppIndexer::new(vec![dir.clone()]).build_index();
        assert!(report.skipped.is_empty());
        let entry = &report.entries[0];
        assert_eq!(entry.id, "org.gnome.Terminal");
        assert_eq!(entry.name, "Terminal");
        assert_eq!(entry.exec, "kgx %U");
        assert_eq!(entry.icon, Some("utilities-terminal".to_owned()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn excludes_hidden_entries() {
        let dir = temp_dir("hidden");
        std::fs::write(dir.join("visible.desktop"), valid_entry()).unwrap();
        std::fs::write(
            dir.join("hidden.desktop"),
            "[Desktop Entry]\nType=Application\nName=Hidden\nExec=hidden\nNoDisplay=true\n",
        )
        .unwrap();

        let report = AppIndexer::new(vec![dir.clone()]).build_index();
        assert_eq!(ids(&report), vec!["visible"]);
        let hidden = report
            .skipped
            .iter()
            .find(|skip| skip.path.file_name().unwrap_or_default() == "hidden.desktop")
            .expect("hidden entry should be reported as skipped");
        assert!(hidden.error.to_string().contains("NoDisplay=true"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn user_directory_overrides_system_directory() {
        let system = temp_dir("system");
        let user = temp_dir("user");
        std::fs::write(system.join("shared.desktop"), valid_entry()).unwrap();
        std::fs::write(
            user.join("shared.desktop"),
            "[Desktop Entry]\nType=Application\nName=User Terminal\nExec=user-kgx %U\n",
        )
        .unwrap();

        // `standard` ordering: user first.
        let report = AppIndexer::new(vec![user.clone(), system.clone()]).build_index();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].id, "shared");
        assert_eq!(report.entries[0].name, "User Terminal");
        assert_eq!(report.entries[0].exec, "user-kgx %U");

        // Reversed order documents the precedence rule: the earliest
        // directory in the vector wins regardless of which one it is.
        let report = AppIndexer::new(vec![system.clone(), user.clone()]).build_index();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].name, "Terminal");

        let _ = std::fs::remove_dir_all(&system);
        let _ = std::fs::remove_dir_all(&user);
    }

    #[test]
    fn missing_directory_does_not_abort_and_is_reported() {
        let dir = temp_dir("missing");
        std::fs::write(dir.join("ok.desktop"), valid_entry()).unwrap();

        let report = AppIndexer::new(vec![dir.join("does-not-exist"), dir.clone()]).build_index();
        assert_eq!(ids(&report), vec!["ok"]);
        assert!(
            report
                .skipped
                .iter()
                .any(|skip| matches!(skip.error, IndexerError::DirectoryRead { .. }))
        );

        // Only missing directories: empty index, no crash, one report each.
        let report = AppIndexer::new(vec![
            dir.join("missing-1"),
            dir.join("missing-2"),
        ])
        .build_index();
        assert!(report.entries.is_empty());
        assert_eq!(report.skipped.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_file_is_skipped_without_aborting() {
        let dir = temp_dir("malformed");
        std::fs::write(dir.join("good.desktop"), valid_entry()).unwrap();
        std::fs::write(
            dir.join("bad.desktop"),
            "Type=Application\n[Desktop Entry\nName=Broken\n",
        )
        .unwrap();

        let report = AppIndexer::new(vec![dir.clone()]).build_index();
        assert_eq!(ids(&report), vec!["good"]);
        assert!(
            report
                .skipped
                .iter()
                .any(|skip| matches!(skip.error, IndexerError::Parse { .. }))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ignores_non_desktop_files_and_directories() {
        let dir = temp_dir("misc");
        std::fs::write(dir.join("app.desktop"), valid_entry()).unwrap();
        std::fs::write(dir.join("notes.txt"), "not a desktop entry\n").unwrap();
        std::fs::create_dir(dir.join("nested")).unwrap();

        let report = AppIndexer::new(vec![dir.clone()]).build_index();
        assert_eq!(ids(&report), vec!["app"]);
        assert!(report.skipped.is_empty(), "non-desktop files must be ignored silently");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_directory_produces_an_empty_index() {
        let dir = temp_dir("empty");
        let report = AppIndexer::new(vec![dir.clone()]).build_index();
        assert!(report.entries.is_empty());
        assert!(report.skipped.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn standard_directories_are_user_first() {
        let directories = standard_directories();
        assert!(!directories.is_empty());
        let last = directories.last().unwrap();
        assert_eq!(last, Path::new("/usr/share/applications"));
        // The user directory precedes the system one.
        for directory in &directories[..directories.len() - 1] {
            assert!(
                directory.to_string_lossy().ends_with("applications"),
                "user dir {directory:?} should end with 'applications'"
            );
        }
    }

    #[test]
    fn flatpak_directories_are_standard_export_paths_only() {
        // The Flatpak export dirs (when they exist) must look like flatpak
        // export paths; nothing else is ever offered.
        for directory in flatpak_directories() {
            assert!(
                directory.to_string_lossy().contains("flatpak/exports/share/applications"),
                "unexpected flatpak dir: {directory:?}"
            );
        }
    }
}