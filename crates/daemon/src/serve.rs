//! Async Unix socket server: accepts `launcherctl` connections and forwards
//! parsed commands to the GTK main loop.

use launcher_core::ipc;
use launcher_ui::CommandHandle;
use std::sync::mpsc::Receiver;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Accept connections until the process is shut down.
///
/// Connection requests are queued until the GTK window has been created, so
/// a command is never dispatched before the main loop is running.
pub async fn serve(listener: UnixListener, ready: Receiver<()>, handle: CommandHandle) {
    tokio::task::spawn_blocking(move || {
        let _ = ready.recv();
    })
    .await
    .ok();

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let handle = handle.clone();
                tokio::spawn(async move {
                    handle_connection(stream, handle).await;
                });
            }
            Err(err) => {
                eprintln!("launcher-daemon: accept: {err}");
            }
        }
    }
}

/// Serve one client: read command lines, answer `OK`/`ERR`, dispatch the
/// parsed command to the GTK thread. Connection errors end the client.
async fn handle_connection(stream: UnixStream, handle: CommandHandle) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => return,
            Ok(_) => {
                let response = match ipc::Command::parse(line.trim()) {
                    Ok(command) => {
                        handle.dispatch(command);
                        "OK".to_owned()
                    }
                    Err(message) => format!("ERR {message}"),
                };
                let _ = reader.get_mut().write_all(response.as_bytes()).await;
                let _ = reader.get_mut().write_all(b"\n").await;
            }
            Err(err) => {
                eprintln!("launcher-daemon: client read: {err}");
                return;
            }
        }
    }
}