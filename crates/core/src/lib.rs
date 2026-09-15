//! Core logic for the launcher: configuration, the IPC protocol, the Unix
//! socket plumbing, and the application indexer.
//!
//! This crate deliberately contains no GTK and no tokio code; it is shared by
//! the daemon, the CLI client and the UI crate. Application matching lives in
//! [`search`], plugins are delegated to `launcher-plugins`, and
//! [`launcher_state`] merges both into the unified result list the UI shows.

pub mod config;
pub mod exec;
pub mod index;
pub mod ipc;
pub mod launcher_state;
pub mod search;
pub mod socket;