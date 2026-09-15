//! CLI client for the launcher daemon.
//!
//! Usage: `launcherctl [toggle|show|hide|quit]` (defaults to `toggle`).
//!
//! The socket path follows [`launcher_core::socket::default_socket_path`],
//! or can be overridden with the `LAUNCHER_SOCKET` environment variable.

use launcher_core::ipc::Command;
use launcher_core::socket::{self, SocketError};
use std::path::PathBuf;

fn main() {
    let command = match parse_args() {
        Ok(command) => command,
        Err(message) => {
            eprintln!("launcherctl: {message}");
            eprintln!("usage: launcherctl [toggle|show|hide|quit]");
            std::process::exit(1);
        }
    };

    let path = match socket_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("launcherctl: could not resolve socket path: {err}");
            std::process::exit(1);
        }
    };

    match socket::send_command(&path, command) {
        Ok(response) => {
            println!("{response}");
            if response == "OK" {
                std::process::exit(0);
            }
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("launcherctl: {err}");
            eprintln!("launcherctl: is the daemon running? start it with `launcher-daemon`");
            std::process::exit(1);
        }
    }
}

/// First positional argument (default `toggle`), mapped onto the protocol.
fn parse_args() -> Result<Command, String> {
    let raw = std::env::args().nth(1).unwrap_or_else(|| "toggle".to_owned());
    Command::parse(&raw).map_err(|message| format!("{message}: '{raw}'"))
}

fn socket_path() -> Result<PathBuf, SocketError> {
    match std::env::var_os("LAUNCHER_SOCKET") {
        Some(path) => Ok(PathBuf::from(path)),
        None => socket::default_socket_path(),
    }
}