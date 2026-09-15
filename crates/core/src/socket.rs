//! Unix domain socket plumbing for the launcher's control interface.
//!
//! The daemon binds a socket in `$XDG_RUNTIME_DIR` (or a `/tmp` fallback) and
//! the `launcherctl` client connects to it. Both sides speak the line
//! protocol from [`crate::ipc`].
//!
//! Security notes:
//! - The socket file is created with mode `0o600`.
//! - The `/tmp` fallback directory is created (and, if needed, fixed to)
//!   mode `0o700`.
//! - A stale socket file left behind by a crashed daemon is detected by
//!   probing the socket and removed before rebinding.

use crate::ipc::Command;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Filename (not path) of the daemon's control socket.
pub const SOCKET_FILE_NAME: &str = "raycast-launcher.sock";

/// Resolve the control socket path: `$XDG_RUNTIME_DIR` preferred, otherwise
/// a private `/tmp/launcher-<user>` directory.
pub fn default_socket_path() -> Result<PathBuf, SocketError> {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(runtime_dir).join(SOCKET_FILE_NAME));
    }

    let user = std::env::var("USER").unwrap_or_else(|_| "user".to_owned());
    let dir = std::env::temp_dir().join(format!("launcher-{user}"));
    ensure_private_dir(&dir)?;
    Ok(dir.join(SOCKET_FILE_NAME))
}

/// Bind the control socket, dealing with stale sockets from crashed daemons.
///
/// Returns [`SocketError::AlreadyRunning`] if another live daemon currently
/// holds the socket.
pub fn bind(path: &Path) -> Result<UnixListener, SocketError> {
    match UnixStream::connect(path) {
        Ok(_) => return Err(SocketError::AlreadyRunning(path.to_path_buf())),
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => {
            // Stale socket from a crashed daemon: remove it and rebind.
            let _ = std::fs::remove_file(path);
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(path)?;
    set_socket_permissions(path)?;
    Ok(listener)
}

/// Remove `path` (typically the socket file) when dropped.
pub struct SocketGuard {
    path: PathBuf,
}

impl SocketGuard {
    /// Wrap a socket path so it is removed on drop.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Send one [`Command`] to the daemon and return the single response line.
pub fn send_command(path: &Path, command: Command) -> Result<String, SocketError> {
    let mut stream = UnixStream::connect(path)?;
    writeln!(stream, "{command}")?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut response = String::new();
    BufReader::new(stream).read_line(&mut response)?;
    Ok(response.trim().to_owned())
}

fn ensure_private_dir(dir: &Path) -> Result<(), SocketError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(dir)?;
    let perms = std::fs::metadata(dir)?.permissions();
    if perms.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_socket_permissions(path: &Path) -> Result<(), SocketError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// Errors that can occur while setting up or using the control socket.
#[derive(Debug)]
pub enum SocketError {
    /// An I/O error occurred.
    Io(std::io::Error),
    /// Another live daemon already owns the socket.
    AlreadyRunning(PathBuf),
}

impl fmt::Display for SocketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SocketError::Io(err) => write!(f, "{err}"),
            SocketError::AlreadyRunning(path) => {
                write!(f, "another daemon is already running on {}", path.display())
            }
        }
    }
}

impl std::error::Error for SocketError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SocketError::Io(err) => Some(err),
            SocketError::AlreadyRunning(_) => None,
        }
    }
}

impl From<std::io::Error> for SocketError {
    fn from(err: std::io::Error) -> Self {
        SocketError::Io(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("launcher-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn first_bind_succeeds_and_second_is_rejected() {
        let dir = scratch_dir("double");
        let path = dir.join("double.sock");

        let listener = bind(&path).expect("first bind should succeed");
        assert!(path.exists());

        match bind(&path) {
            Err(SocketError::AlreadyRunning(_)) => {}
            other => panic!("second bind should report AlreadyRunning, got {other:?}"),
        }

        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bind_recovers_from_stale_socket() {
        let dir = scratch_dir("stale");
        let path = dir.join("stale.sock");

        // Simulate a crashed daemon by binding and then abandoning a raw
        // socket file (the probe detects ConnectionRefused and removes it).
        let stale = UnixListener::bind(&path).unwrap();
        drop(stale);
        assert!(path.exists());

        let listener = bind(&path).expect("stale socket should be recovered");
        assert!(path.exists());

        drop(listener);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn send_command_round_trips_a_response() {
        let dir = scratch_dir("roundtrip");
        let path = dir.join("roundtrip.sock");

        let listener = bind(&path).unwrap();
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut reader = BufReader::new(stream.try_clone()?);
            let mut line = String::new();
            reader.read_line(&mut line)?;
            assert_eq!(line.trim(), "TOGGLE");
            stream.write_all(b"OK\n")
        });

        let response = send_command(&path, Command::Toggle).expect("send_command should work");
        assert_eq!(response, "OK");
        server.join().unwrap().unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}