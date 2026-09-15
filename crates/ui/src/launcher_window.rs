//! The launcher window and its control surface.
//!
//! [`LauncherWindow`] owns the GTK widgets and the interactions between them:
//! the search box forwards text changes, the results list mirrors the shared
//! [`LauncherState`], and keyboard navigation maps onto state/selection moves.
//! The window itself is kept alive for the whole daemon lifetime and merely
//! shown/hidden on each activation.
//!
//! [`LauncherControl`] is the object the crate-external IPC path touches: it
//! holds the window behind the scenes and simply forwards `show`/`hide`/
//! `toggle` requests. Every public method is safe to call only on the GTK main
//! thread — which the crate already guarantees via [`CommandHandle`].
//!
//! [`CommandHandle`]: crate::CommandHandle

use gtk4::gdk::Key;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, EventControllerKey, PropagationPhase};
use launcher_core::config::Appearance;
use launcher_core::exec;
use launcher_core::index::AppEntry;
use launcher_core::launcher_state::LauncherState;
use launcher_plugins::PluginAction;
use launcher_plugins::PluginRegistry;
use launcher_plugins::clipboard::ClipboardHistory;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::appearance;
use crate::{results, search};

/// Fade-in pacing: `FADE_STEP_MS` milliseconds between `FADE_FRAMES` opacity
/// steps. 120 ms total, in line with the Apple-style motion guidance.
const FADE_STEP_MS: u64 = 12;
const FADE_FRAMES: u32 = 10;

/// Window height the search box + default results fit into without scrolling.
const WINDOW_HEIGHT: i32 = 460;

/// The launcher window: search box, results list and the shared state.
pub struct LauncherWindow {
    window: ApplicationWindow,
    entry: gtk4::SearchEntry,
    /// Content container. Its CSS class drives the scale-in transition.
    surface: gtk4::Box,
    results: Rc<results::ResultsView>,
    /// The system clipboard this launcher reads and writes. Pointing at the
    /// real clipboard object would make tests clobber the session's clipboard
    /// (and `ClipboardHistory` unit tests could not assert reliably); tests
    /// substitute it with a fake. The window it belongs to owns a reference
    /// only; the construction is delegated to [`ClipboardHistory::sink`].
    clipboard: (),
    state: RefCell<LauncherState>,
    /// Monotonic generation counter for the fade-in timeline; bumping it
    /// cancels any in-flight fade (e.g. show while still fading out).
    fade_generation: Rc<RefCell<u64>>,
}

/// Thread-safe-ish handle used by the IPC layer to command the window.
/// Must be used on the GTK main thread, which the crate guarantees.
pub struct LauncherControl {
    target: RefCell<Option<Rc<LauncherWindow>>>,
}

impl LauncherControl {
    /// Create an empty control; attach the window once it exists.
    #[must_use]
    pub fn new() -> Self {
        Self {
            target: RefCell::new(None),
        }
    }

    /// Attach the window backing this control (called once at startup).
    pub fn attach(&self, window: Rc<LauncherWindow>) {
        *self.target.borrow_mut() = Some(window);
    }

    /// Show the window: clear the query, reset the results, re-focus the
    /// search box and fade in.
    pub fn show(&self) {
        self.with_window(LauncherWindow::show);
    }

    /// Hide the window until the next activation.
    pub fn hide(&self) {
        self.with_window(LauncherWindow::hide);
    }

    /// Show when hidden, hide when shown.
    pub fn toggle(&self) {
        self.with_window(LauncherWindow::toggle);
    }

    fn with_window(&self, action: impl FnOnce(&LauncherWindow)) {
        if let Some(window) = self.target.borrow().clone() {
            action(&window);
        }
    }
}

impl Default for LauncherControl {
    fn default() -> Self {
        Self::new()
    }
}

impl LauncherWindow {
    /// Build the window. The widgets are created once; from here on the
    /// window lives for the daemon's lifetime and is only hidden/shown.
    #[must_use]
    pub fn build(
        app: &Application,
        appearance: &Appearance,
        theme: &str,
        apps: Vec<AppEntry>,
        plugins: PluginRegistry,
        history: Arc<Mutex<ClipboardHistory>>,
        history_path: Option<PathBuf>,
    ) -> Rc<Self> {
        let results = results::ResultsView::new();
        let entry = search::new();

        // The daemon pre-sets the retention (from config) and loads the file
        // when persistence is on, which is exactly when `history_path` is
        // set. A session-only run must keep history memory-only even if the
        // config flipped while the daemon is alive, so pin the opposite
        // decision onto the shared history here.
        if let Ok(mut history) = history.lock() {
            if history_path.is_none() && history.retention_seconds().is_some() {
                history.set_retention_seconds(None);
            }
        } else {
            eprintln!("launcher: clipboard history lock poisoned");
        }

        let window = ApplicationWindow::builder()
            .application(app)
            .title("Launcher")
            .decorated(false)
            .resizable(false)
            .default_width(appearance.window_width.clamp(320, 1200) as i32)
            .default_height(WINDOW_HEIGHT)
            .build();
        window.add_css_class("launcher");

        appearance::apply(&window, appearance, theme);

        let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        vbox.add_css_class("launcher-surface");
        vbox.append(&entry);
        vbox.append(results.widget());
        window.set_child(Some(&vbox));

        let this = Rc::new(Self {
            window,
            entry,
            surface: vbox,
            results,
            clipboard: ClipboardHistory::sink(),
            state: RefCell::new(LauncherState::with_plugins(apps, plugins)),
            fade_generation: Rc::new(RefCell::new(0)),
        });

        this.connect_query_changes();
        this.connect_key_controller();
        this.connect_close_request();

        this
    }

    /// Show the launcher: visible, cleared, focused and faded/scaled in.
    pub fn show(&self) {
        self.state.borrow_mut().set_visible(true);
        // Clearing the box fires `search-changed`, resetting query/results.
        search::clear(&self.entry);
        self.entry.grab_focus();
        // Start scaled down; `animate_opacity` drops this class once the
        // window is mapped so the CSS transition plays from 96% up to 100%.
        self.surface.add_css_class("launcher-scale-in");
        self.window.present();
        self.window.set_opacity(0.0);
        self.animate_opacity(1.0);
    }

    /// Hide the launcher until the next activation.
    pub fn hide(&self) {
        self.state.borrow_mut().set_visible(false);
        self.window.set_visible(false);
    }

    /// Show when hidden, hide when shown.
    pub fn toggle(&self) {
        if self.state.borrow().is_visible() {
            self.hide();
        } else {
            self.show();
        }
    }

    /// The search text changed: recompute results and re-highlight.
    fn query_changed(&self, query: &str) {
        self.state.borrow_mut().set_query(query);
        self.results.update(&self.state.borrow());
    }

    /// Move the highlight by `delta` (−1/+1) within the current results.
    fn move_selection(&self, delta: isize) {
        self.state.borrow_mut().move_selection(delta);
        self.results.highlight(self.state.borrow().selection());
    }

    /// Activate the highlighted row: run a plugin's action (e.g. copy the
    /// calculator result), or launch the highlighted application. Ends by
    /// hiding and resetting the box. Launching never blocks; a spawn failure
    /// just reports to stderr and resets the query so the user can try again.
    fn activate(&self) {
        let launch_error = match self.clipboard_action() {
            Some(()) => {
                self.hide_and_clear();
                return;
            }
            None => match self.state.borrow().selected_app() {
                Some(app) => match exec::launch(&app.exec) {
                    Ok(()) => {
                        self.hide_and_clear();
                        return;
                    }
                    Err(err) => Some(format!("could not launch {}: {err}", app.name)),
                },
                None => None,
            },
        };
        self.state.borrow_mut().clear();
        search::clear(&self.entry);
        if let Some(message) = launch_error {
            eprintln!("launcher: {message}");
        }
    }

    /// Execute the highlighted row's plugin action, when it has one
    /// (`PluginAction::Copy` puts the text on the system clipboard). Returns
    /// `Some(())` when an action ran, `None` when the row has none.
    fn clipboard_action(&self) -> Option<()> {
        let action = self.state.borrow().selected_plugin_action()?;
        let PluginAction::Copy { text } = action;
        // The real clipboard lives on the window; the field is the sink the
        // window was built with (tests swap in a fake there).
        let _ = self.clipboard;
        self.window.clipboard().set_text(&text);
        Some(())
    }

    /// Hide the window and reset the search box (next activation starts clean).
    fn hide_and_clear(&self) {
        self.state.borrow_mut().set_visible(false);
        self.state.borrow_mut().clear();
        search::clear(&self.entry);
        self.window.set_visible(false);
    }

    /// Fade the window opacity from its current value to `target` in fixed
    /// steps. Once the window is mapped, the first step also drops the
    /// scale-in class so the CSS transform transition runs in parallel. A
    /// newer fade bumps the generation and cancels this timeline.
    fn animate_opacity(&self, target: f64) {
        *self.fade_generation.borrow_mut() += 1;
        let generation = *self.fade_generation.borrow();
        let start = self.window.opacity();
        if (start - target).abs() < 0.005 {
            self.window.set_opacity(target);
            return;
        }
        let step = (target - start) / f64::from(FADE_FRAMES);
        let frames = FADE_FRAMES;
        let window = self.window.clone();
        let surface = self.surface.clone();
        let generation_cell = self.fade_generation.clone();
        let mut frame = 0u32;
        glib::source::timeout_add_local(
            Duration::from_millis(FADE_STEP_MS),
            move || {
                if *generation_cell.borrow() != generation {
                    return glib::ControlFlow::Break;
                }
                frame += 1;
                // Give the compositor a frame or two to map the window before
                // triggering the scale transition from its initial state.
                if frame == 2 {
                    surface.remove_css_class("launcher-scale-in");
                }
                if frame >= frames {
                    window.set_opacity(target);
                    glib::ControlFlow::Break
                } else {
                    window.set_opacity(start + step * f64::from(frame));
                    glib::ControlFlow::Continue
                }
            },
        );
    }

    /// Forward `search-changed` events from the entry to the state/results.
    fn connect_query_changes(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        search::connect_changed(&self.entry, move |text| {
            if let Some(window) = weak.upgrade() {
                window.query_changed(text);
            }
        });
    }

    /// Handle the launcher's keyboard shortcuts. Capture phase so the window
    /// wins over the entry's own key handling; everything else falls through.
    fn connect_key_controller(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let controller = EventControllerKey::new();
        controller.set_propagation_phase(PropagationPhase::Capture);
        controller.connect_key_pressed(move |_, key, _keycode, _state| {
            let Some(window) = weak.upgrade() else {
                return glib::Propagation::Stop;
            };
            match key {
                Key::Escape => {
                    window.hide_and_clear();
                    glib::Propagation::Stop
                }
                Key::Up => {
                    window.move_selection(-1);
                    glib::Propagation::Stop
                }
                Key::Down => {
                    window.move_selection(1);
                    glib::Propagation::Stop
                }
                Key::Return | Key::KP_Enter | Key::ISO_Enter => {
                    window.activate();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.window.add_controller(controller);
    }

    /// Closing the window (compositor close button, Alt+F4, ...) only hides
    /// it; the daemon stays alive until it receives `Command::Quit`.
    fn connect_close_request(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |window| {
            if let Some(w) = weak.upgrade() {
                w.state.borrow_mut().set_visible(false);
            }
            window.set_visible(false);
            glib::Propagation::Stop
        });
    }
}