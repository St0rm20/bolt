//! GTK4 user interface for the launcher.
//!
//! The launcher is a single, long-lived floating window driven by two halves:
//!
//! * a pure, GTK-free **search model** in `launcher-core` ([`LauncherState`])
//!   that owns the application index, the query and the highlighted row, and
//! * a thin GTK **view** in this crate that renders that state. View wiring
//!   lives in [`appearance`], [`search`], [`results`] and [`launcher_window`].
//!
//! The daemon runs this crate's [`launch`] on a dedicated GTK thread and feeds
//! it commands over a thread-safe [`CommandHandle`] (see the module docs on
//! [`CommandHandle::dispatch`] for the threading model). Launching
//! applications happens through the GTK-free `exec` module of `launcher-core`,
//! so the UI never parses `Exec` lines itself.

pub mod appearance;
pub mod launcher_window;
pub mod results;
pub mod search;

use gio::prelude::*;
use gtk4::Application;
use gtk4::glib;
use launcher_core::config::Config;
use launcher_core::index::AppEntry;
use launcher_core::ipc::Command;
use launcher_plugins::PluginRegistry;
use launcher_window::{LauncherControl, LauncherWindow};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

/// Default GLib application identifier.
pub const DEFAULT_APP_ID: &str = "dev.launcher.gtk";

// GTK object handles are not `Send`, so they stay inside thread-local slots on
// the GTK thread. `CommandHandle` is the only cross-thread bridge: its
// `dispatch` hands each command to the GTK main loop with `MainContext::invoke`
// and the callback only ever touches these thread-local statics plus a plain
// `Command` value.
thread_local! {
    static CONTROL_SLOT: RefCell<Option<Rc<LauncherControl>>> = const { RefCell::new(None) };
    static APP_SLOT: RefCell<Option<gtk4::Application>> = const { RefCell::new(None) };
}

/// Thread-safe handle to command the GTK launcher.
///
/// The daemon owns one and clones it for the IPC layer; all clones queue work
/// onto the same GTK main loop through [`glib::MainContext::invoke`].
#[derive(Clone)]
pub struct CommandHandle {
    context: glib::MainContext,
    ready: mpsc::Sender<()>,
}

impl CommandHandle {
    /// Create a handle together with a channel that fires once the launcher
    /// window has been created inside [`launch`].
    pub fn new() -> (Self, mpsc::Receiver<()>) {
        let (ready, ready_rx) = mpsc::channel();
        (
            Self {
                context: glib::MainContext::default(),
                ready,
            },
            ready_rx,
        )
    }

    /// Queue a [`Command`] to run on the GTK main thread.
    ///
    /// Returns immediately; the command is executed later by the main loop.
    /// Safe to call from any thread.
    pub fn dispatch(&self, command: Command) {
        let context = self.context.clone();
        context.invoke(move || apply_command(command));
    }

    /// Signal that the launcher window exists (called once by [`launch`]).
    fn ready(&self) {
        let _ = self.ready.send(());
    }
}

/// Apply a single [`Command`] on the GTK main thread.
fn apply_command(command: Command) {
    match command {
        Command::Toggle => with_control(|control| control.toggle()),
        Command::Show => with_control(|control| control.show()),
        Command::Hide => with_control(|control| control.hide()),
        Command::Quit => {
            with_control(|control| control.hide());
            if let Some(app) = app_slot() {
                app.quit();
            }
        }
    }
}

/// Run an action against the current window, if one exists yet.
fn with_control(action: impl FnOnce(&LauncherControl)) {
    CONTROL_SLOT.with(|slot| {
        if let Some(control) = slot.borrow().clone() {
            action(&control);
        }
    });
}

/// The running GTK application, if any.
fn app_slot() -> Option<Application> {
    APP_SLOT.with(|slot| slot.borrow().clone())
}

/// Build and run the GTK launcher application, blocking until it quits.
///
/// The daemon hands over the compiled application index (`apps`), the plugin
/// registry to serve prefix queries with, the appearance configuration and a
/// [`CommandHandle`] to drive the window. The index is consumed exactly once,
/// when the window is first activated; commands arriving before that (the
/// daemon gates on the `ready` receiver) are no-ops.
///
/// Returns the application's exit code.
pub fn launch(
    application_id: &str,
    config: &Config,
    apps: Vec<AppEntry>,
    plugins: PluginRegistry,
    handle: CommandHandle,
) -> glib::ExitCode {
    let appearance = config.appearance.clone();
    let theme = config.effective_theme().to_owned();
    let apps_cell = Rc::new(RefCell::new(Some(apps)));
    let plugins_cell = Rc::new(RefCell::new(Some(plugins)));

    let app = Application::builder().application_id(application_id).build();
    APP_SLOT.with(|slot| *slot.borrow_mut() = Some(app.clone()));

    app.connect_activate(move |app| {
        // Take the index and plugins once; later `activate` calls (rare, e.g.
        // a second activation while the window already exists) reuse the
        // built window.
        let (Some(apps), Some(plugins)) = (apps_cell.borrow_mut().take(), plugins_cell.borrow_mut().take())
        else {
            return;
        };
        let window = LauncherWindow::build(app, &appearance, &theme, apps, plugins);
        let control = Rc::new(LauncherControl::new());
        control.attach(window);
        CONTROL_SLOT.with(|slot| *slot.borrow_mut() = Some(control));
        handle.ready();
    });

    app.run()
}

/// Convenience alias for tests.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_dispatches_without_a_window() {
        let (handle, _ready) = CommandHandle::new();
        handle.dispatch(Command::Toggle);
        handle.dispatch(Command::Show);
        handle.dispatch(Command::Hide);
        handle.dispatch(Command::Quit);
    }
}
