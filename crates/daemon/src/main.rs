//! Resident launcher daemon.
//!
//! Owns the tokio runtime and the control socket, forwards commands to the
//! GTK main loop through [`launcher_ui::CommandHandle`], and lets Hyprland
//! (or anything else) drive it via `launcherctl`.

mod serve;

use launcher_core::config::Config;
use launcher_core::index::AppIndexer;
use launcher_core::ipc::Command;
use launcher_core::socket::{self, SocketError, SocketGuard};
use launcher_plugins::calculator::CalculatorPlugin;
use launcher_plugins::clipboard::{
    default_history_path, load_into, ArboardClipboardSource, ClipboardHistory, ClipboardMonitor,
    ClipboardPlugin, LoadOutcome,
};
use launcher_plugins::echo::EchoPlugin;
use launcher_plugins::PluginRegistry;
use launcher_ui::{CommandHandle, DEFAULT_APP_ID};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::UnixListener as TokioUnixListener;
use tokio::signal::unix::{signal, SignalKind};

/// Default location of the configuration file, relative to the directory the
/// daemon is started from. Override with the `LAUNCHER_CONFIG` environment
/// variable.
const DEFAULT_CONFIG_PATH: &str = "config/config.toml";

fn config_path() -> PathBuf {
    std::env::var_os("LAUNCHER_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH))
}

fn load_config() -> Config {
    let path = config_path();
    match Config::load(&path) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("launcher-daemon: could not load {}: {err}", path.display());
            eprintln!("launcher-daemon: using defaults");
            Config::default()
        }
    }
}

/// The registry the daemon serves: built-in plugins, filtered by the
/// `enabled_plugins` config allow-list when one is present. The clipboard
/// plugin receives the same shared history the monitor thread keeps updated.
fn build_plugin_registry(
    config: &Config,
    clipboard_history: &Arc<Mutex<ClipboardHistory>>,
) -> PluginRegistry {
    let mut registry = PluginRegistry::new();
    registry.register(Box::new(EchoPlugin));
    registry.register(Box::new(CalculatorPlugin));
    registry.register(Box::new(ClipboardPlugin::new(clipboard_history.clone())));
    if !config.enabled_plugins.is_empty() {
        registry.retain(|plugin| {
            config.enabled_plugins.iter().any(|id| id == plugin.id())
        });
    }
    registry
}

fn spawn_clipboard_monitor(
    history: Arc<Mutex<ClipboardHistory>>,
    persistence: Option<PathBuf>,
    running: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    let handle = std::thread::Builder::new()
        .name("clipboard-monitor".to_owned())
        .spawn(move || {
            // The source is created in-thread. Without a Wayland data-control
            // backend the monitor reports and stops; the daemon keeps running.
            let source = match ArboardClipboardSource::new() {
                Ok(source) => source,
                Err(err) => {
                    eprintln!("launcher-daemon: clipboard capture unavailable: {err}");
                    return;
                }
            };
            let mut monitor = ClipboardMonitor::new(Box::new(source), history, persistence);
            monitor.run(&running);
        });
    match handle {
        Ok(handle) => Some(handle),
        Err(err) => {
            eprintln!("launcher-daemon: could not start clipboard monitor: {err}");
            None
        }
    }
}

fn main() {
    let config = load_config();
    let config_file = config_path();

    // Clipboard history is shared between the `clip:` plugin (searches it)
    // and the monitor thread (fills it from the system clipboard). The
    // retention from the config bounds entry age and decides whether the
    // history is persisted at all (`session` keeps it memory-only).
    let retention_seconds = config.clipboard.retention.as_seconds();
    let clipboard_history = Arc::new(Mutex::new(ClipboardHistory::new()));
    clipboard_history
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .set_retention_seconds(retention_seconds);
    if retention_seconds.is_some() {
        let path = default_history_path();
        let mut history = clipboard_history.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match load_into(&mut history, &path) {
            LoadOutcome::Loaded(count) => eprintln!(
                "launcher-daemon: clipboard history: {count} entries loaded from {}",
                path.display()
            ),
            // No file yet is a normal first run; corrupt/unreadable files are
            // reported but never fatal — the session simply starts empty.
            LoadOutcome::Missing => {}
            LoadOutcome::Corrupted => eprintln!(
                "launcher-daemon: clipboard history at {} is corrupted; starting empty",
                path.display()
            ),
            LoadOutcome::Unreadable(err) => eprintln!(
                "launcher-daemon: could not read clipboard history {}: {err}",
                path.display()
            ),
        }
    }

    let plugins = build_plugin_registry(&config, &clipboard_history);

    let persistence = retention_seconds.is_some().then(default_history_path);
    let clipboard_running = Arc::new(AtomicBool::new(true));
    let clipboard_thread = spawn_clipboard_monitor(
        clipboard_history.clone(),
        persistence.clone(),
        clipboard_running.clone(),
    );

    let socket_path = match socket::default_socket_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("launcher-daemon: could not resolve socket path: {err}");
            std::process::exit(1);
        }
    };

    let listener = match socket::bind(&socket_path) {
        Ok(listener) => listener,
        Err(SocketError::AlreadyRunning(path)) => {
            eprintln!("launcher-daemon: another instance is running on {}", path.display());
            std::process::exit(2);
        }
        Err(err) => {
            eprintln!("launcher-daemon: could not bind {}: {err}", socket_path.display());
            std::process::exit(1);
        }
    };
    // The socket file is removed when the daemon exits.
    let _guard = SocketGuard::new(&socket_path);

    // Thread-safe handle so tokio tasks can command the GTK main loop.
    let (handle, ready) = CommandHandle::new();

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("launcher-daemon: could not build runtime: {err}");
            std::process::exit(1);
        }
    };

    eprintln!("launcher-daemon: listening on {}", socket_path.display());
    eprintln!(
        "launcher-daemon: theme = {}, shortcut = {}",
        config.effective_theme(),
        config.shortcut
    );
    let plugin_names: Vec<&str> = plugins.plugins().iter().map(|plugin| plugin.id()).collect();
    eprintln!("launcher-daemon: plugins: {}", plugin_names.join(", "));

    // Build the in-memory application index once, before the GTK loop starts.
    // Apps that could not be parsed or were filtered out are reported (they
    // are skipped from the search, nothing more).
    let report = AppIndexer::standard().build_index();
    let apps = report.entries;
    let skipped = report.skipped;
    eprintln!(
        "launcher-daemon: indexed {} applications ({} skipped)",
        apps.len(),
        skipped.len()
    );

    // Everything runs inside the runtime so the listener can be converted to
    // tokio's non-blocking form. The spawned tasks run on worker threads
    // while this thread runs the blocking GTK main loop.
    runtime.block_on(async {
        let listener = match listener.set_nonblocking(true) {
            Ok(()) => match TokioUnixListener::from_std(listener) {
                Ok(listener) => listener,
                Err(err) => {
                    eprintln!("launcher-daemon: could not import listener: {err}");
                    std::process::exit(1);
                }
            },
            Err(err) => {
                eprintln!("launcher-daemon: could not prepare listener: {err}");
                std::process::exit(1);
            }
        };

        runtime.spawn(serve::serve(listener, ready, handle.clone()));
        runtime.spawn(forward_signals(handle.clone()));

        launcher_ui::launch(
            DEFAULT_APP_ID,
            &config,
            apps,
            plugins,
            handle,
            clipboard_history,
            persistence.clone(),
            config_file,
        );
    });

    // The GTK loop has ended (Command::Quit): stop the clipboard monitor and
    // let it flush its final state before the daemon exits.
    clipboard_running.store(false, Ordering::Relaxed);
    if let Some(thread) = clipboard_thread {
        let _ = thread.join();
    }
}

/// Forward SIGINT/SIGTERM to the GTK main loop as a [`Command::Quit`].
async fn forward_signals(handle: CommandHandle) {
    let mut terminate = signal(SignalKind::terminate()).expect("SIGTERM handler");
    let mut interrupt = signal(SignalKind::interrupt()).expect("SIGINT handler");
    tokio::select! {
        _ = terminate.recv() => {}
        _ = interrupt.recv() => {}
    }
    handle.dispatch(Command::Quit);
}