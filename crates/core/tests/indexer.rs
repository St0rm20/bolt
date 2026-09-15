//! Integration tests for the launcher indexer.
//!
//! These tests use the committed fixtures in `tests/fixtures/`; they never
//! read the host system's `/usr/share/applications` or `~/.local/share/applications`,
//! so results are deterministic and portable.

use launcher_core::index::{AppIndexer, IndexerError};
use std::path::{Path, PathBuf};

/// Resolve a path below `crates/core/tests/fixtures`.
fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(relative)
}

/// Index all ids, for compact assertions.
fn ids(report: &launcher_core::index::IndexReport) -> Vec<String> {
    report.entries.iter().map(|entry| entry.id.clone()).collect()
}

/// The skip record for `file_name`, if any.
fn skipped_named<'a>(
    report: &'a launcher_core::index::IndexReport,
    file_name: &str,
) -> Option<&'a launcher_core::index::Skipped> {
    report
        .skipped
        .iter()
        .find(|skip| skip.path.file_name().map_or(false, |name| name == file_name))
}

/// Test 1 — a valid desktop entry yields an `AppEntry` with id, name, exec
/// and icon.
#[test]
fn valid_application_is_indexed() {
    let directories = vec![fixture("applications")];
    let report = AppIndexer::new(directories).build_index();

    let entry = report
        .entries
        .iter()
        .find(|entry| entry.id == "valid-app")
        .expect("valid-app should be indexed");

    assert_eq!(entry.name, "Valid App");
    // `Exec` retains placeholders and flags, and is never executed.
    assert_eq!(entry.exec, "/usr/bin/valid-app --flag %U");
    assert_eq!(entry.icon.as_deref(), Some("applications-graphics"));
}

/// Test 2 — `NoDisplay=true` entries are excluded from the index.
#[test]
fn hidden_application_is_excluded() {
    let report = AppIndexer::new(vec![fixture("applications")]).build_index();

    assert!(
        !ids(&report).iter().any(|id| id == "hidden-app"),
        "NoDisplay=true entry must be excluded from the index"
    );
    let hidden = skipped_named(&report, "hidden-app.desktop");
    let error = hidden.expect("hidden-app should be reported as skipped").error.to_string();
    assert!(error.contains("NoDisplay=true"), "unexpected reason: {error}");
}

/// Test 3 — a malformed entry is skipped; valid files are still indexed and
/// the build does not panic or abort.
#[test]
fn malformed_entry_is_skipped_without_aborting() {
    let report = AppIndexer::new(vec![fixture("applications")]).build_index();

    assert!(ids(&report).iter().any(|id| id == "valid-app"));
    assert!(skipped_named(&report, "malformed-app.desktop").is_some());
    assert!(matches!(
        skipped_named(&report, "malformed-app.desktop").unwrap().error,
        IndexerError::Parse { .. }
    ));
}

/// Test 4 — a user-local entry with the same desktop entry id overrides the
/// system-wide entry.
#[test]
fn user_local_entry_overrides_system_entry() {
    let directories = vec![fixture("local-override"), fixture("applications")];
    let report = AppIndexer::new(directories).build_index();

    let entry = report
        .entries
        .iter()
        .find(|entry| entry.id == "valid-app")
        .expect("valid-app should be indexed");

    // The user-local fixture wins and the system duplicate is dropped.
    assert_eq!(entry.name, "User Local App");
    assert_eq!(entry.exec, "/usr/bin/local-valid-app %U");
    assert_eq!(entry.icon, None);
}

/// Only `Type=Application` entries are indexed.
#[test]
fn non_application_entries_are_excluded() {
    let report = AppIndexer::new(vec![fixture("applications")]).build_index();

    assert!(
        !ids(&report).iter().any(|id| id == "non-application"),
        "Type=Link must not be indexed"
    );
    assert!(
        skipped_named(&report, "non-application.desktop").is_some(),
        "non-application entry should be reported as skipped"
    );
}

/// Entries missing a required field are excluded, not indexed or panicking.
#[test]
fn entry_without_name_is_excluded() {
    let report = AppIndexer::new(vec![fixture("applications")]).build_index();

    assert!(
        !ids(&report).iter().any(|id| id == "missing-name"),
        "entry without Name must not be indexed"
    );
}

/// Files without the `.desktop` extension are ignored silently.
#[test]
fn non_desktop_files_are_ignored() {
    let report = AppIndexer::new(vec![fixture("applications")]).build_index();

    assert!(
        !skipped_named(&report, "notes.txt").is_some(),
        "notes.txt should be ignored, not reported"
    );
}

/// A Flatpak desktop entry (`Exec` referencing `flatpak run`) is indexed
/// normally with its `Exec` line kept verbatim, so it can be spawned
/// unchanged.
#[test]
fn flatpak_desktop_entries_are_indexed_with_verbatim_exec() {
    let report = AppIndexer::new(vec![fixture("flatpak")]).build_index();

    let entry = report
        .entries
        .iter()
        .find(|entry| entry.id == "com.microsoft.Edge")
        .expect("flatpak desktop entry should be indexed");
    assert_eq!(entry.name, "Microsoft Edge");
    // The `flatpak run ...` line is kept verbatim (field codes and all); the
    // launcher never rewrites it, it just spawns the command.
    assert_eq!(
        entry.exec,
        "/usr/bin/flatpak run --branch=stable --file-forwarding com.microsoft.Edge @@u %U @@"
    );
    assert_eq!(entry.icon.as_deref(), Some("com.microsoft.Edge"));
}

/// The index is deterministic: rebuilding yields the same entries in the same
/// (id-sorted) order.
#[test]
fn rebuild_is_deterministic() {
    let indexer = AppIndexer::new(vec![fixture("applications")]);

    let first = indexer.build_index();
    let second = indexer.build_index();

    assert_eq!(first.entries, second.entries);
    let mut sorted = first.entries.clone();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(first.entries, sorted);
}