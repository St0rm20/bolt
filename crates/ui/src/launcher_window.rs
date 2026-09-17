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
use launcher_core::config::{Appearance, Config, ClipboardRetention};
use launcher_core::exec;
use launcher_core::index::AppEntry;
use launcher_core::launcher_state::{BoltCommand, LauncherState, ListRow};
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
const PENDING_POLL_MS: u64 = 150;

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
    history: Arc<Mutex<ClipboardHistory>>,
    state: RefCell<LauncherState>,
    /// Monotonic generation counter for the fade-in timeline; bumping it
    /// cancels any in-flight fade (e.g. show while still fading out).
    fade_generation: Rc<RefCell<u64>>,
    /// Monotonic generation counter for pending-result polling.
    pending_generation: Rc<RefCell<u64>>,
    /// Back-reference used by the pending-result timer without creating a
    /// reference cycle.
    pending_window: RefCell<std::rc::Weak<LauncherWindow>>,
    config_path: PathBuf,
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
        config_path: PathBuf,
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
            .title("Bolt")
            .decorated(false)
            .resizable(false)
            .default_width(appearance.window_width.clamp(320, 1200) as i32)
            .default_height(WINDOW_HEIGHT)
            .build();
        window.add_css_class("launcher");

        appearance::apply(&window, appearance, theme);

        let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        vbox.add_css_class("launcher-surface");

        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        row.add_css_class("launcher-search-row");
        row.set_valign(gtk4::Align::Center);

        let icon = gtk4::Image::new();
        let icon_path = "icon/bolt.svg";
        if let Some(path) = std::fs::canonicalize(icon_path).ok() {
            icon.set_from_file(Some(path.to_str().unwrap_or(icon_path)));
        } else {
            icon.set_from_file(Some("icon/bolt.png"));
        }
        icon.add_css_class("launcher-brand-icon");
        icon.set_pixel_size(38);
        icon.set_halign(gtk4::Align::Start);
        icon.set_valign(gtk4::Align::Center);
        icon.set_margin_start(10);

        entry.set_valign(gtk4::Align::Center);
        row.append(&icon);
        row.append(&entry);
        vbox.append(&row);
        vbox.append(results.widget());

        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&vbox));
        let close = gtk4::Button::with_label("×");
        close.add_css_class("launcher-close");
        close.set_tooltip_text(Some("Close"));
        close.set_halign(gtk4::Align::End);
        close.set_valign(gtk4::Align::Start);
        close.set_margin_top(8);
        close.set_margin_end(8);
        overlay.add_overlay(&close);

        window.set_child(Some(&overlay));

        let this = Rc::new(Self {
            window,
            entry,
            surface: vbox,
            results,
            clipboard: ClipboardHistory::sink(),
            history: history.clone(),
            state: RefCell::new(LauncherState::with_plugins(apps, plugins)),
            fade_generation: Rc::new(RefCell::new(0)),
            pending_generation: Rc::new(RefCell::new(0)),
            pending_window: RefCell::new(std::rc::Weak::new()),
            config_path,
        });

        *this.pending_window.borrow_mut() = Rc::downgrade(&this);
        this.connect_query_changes();
        this.connect_key_controller();
        this.connect_close_request();
        this.connect_clicks();
        let weak = Rc::downgrade(&this);
        close.connect_clicked(move |_| {
            if let Some(window) = weak.upgrade() {
                window.hide_and_clear();
            }
        });
        this.results.update(&this.state.borrow());

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
        *self.pending_generation.borrow_mut() += 1;
        self.state.borrow_mut().set_query(query);
        self.results.update(&self.state.borrow());
        if self.state.borrow().is_pending() {
            self.poll_pending_results();
        }
    }

    /// Refresh results until the active plugin finishes its async work. A
    /// newer query invalidates this timer through `pending_generation`.
    fn poll_pending_results(&self) {
        let generation = *self.pending_generation.borrow();
        let generation_cell = self.pending_generation.clone();
        let weak = self.pending_window.borrow().clone();
        glib::source::timeout_add_local(Duration::from_millis(PENDING_POLL_MS), move || {
            if *generation_cell.borrow() != generation {
                return glib::ControlFlow::Break;
            }
            let Some(window) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !window.state.borrow().is_visible() {
                return glib::ControlFlow::Break;
            }
            let query = window.entry.text();
            window.state.borrow_mut().set_query(query);
            window.results.update(&window.state.borrow());
            if window.state.borrow().is_pending() {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }

    /// Update selection on row clicks. A click on an unselected row selects it;
    /// a click on the already-selected row acts like Enter.
    fn select_or_activate(&self, index: usize) {
        let selected = self.state.borrow().selection();
        if selected == Some(index) {
            self.activate();
        } else {
            self.state.borrow_mut().set_selection(Some(index));
            self.results.highlight(self.state.borrow().selection());
        }
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
        enum ActivationTarget {
            Command(BoltCommand),
            Plugin,
            App,
            Inactive,
        }

        let target = {
            let state = self.state.borrow();
            state
                .selection()
                .and_then(|index| state.results().get(index))
                .map(|row| match row {
                    ListRow::Command(command) => ActivationTarget::Command(*command),
                    ListRow::Plugin { .. } => ActivationTarget::Plugin,
                    ListRow::App(_) => ActivationTarget::App,
                    ListRow::Hint(_) => ActivationTarget::Inactive,
                })
                .unwrap_or(ActivationTarget::Inactive)
        };

        if let ActivationTarget::Command(command) = target {
            match command {
                BoltCommand::Clipboard => self.query_changed("clip:"),
                BoltCommand::Settings => self.open_settings_window(),
            }
            return;
        }

        match target {
            ActivationTarget::Plugin => match self.plugin_action() {
                Ok(()) => self.hide_and_clear(),
                Err(message) => {
                    self.state.borrow_mut().set_error_hint(message.clone());
                    self.results.update(&self.state.borrow());
                    eprintln!("launcher: {message}");
                }
            },
            ActivationTarget::App => self.launch_selected_app(),
            ActivationTarget::Command(_) | ActivationTarget::Inactive => {}
        }
    }

    /// Launch the selected application and reset the launcher afterward.
    fn launch_selected_app(&self) {
        let launch_error = match self.state.borrow().selected_app() {
            Some(app) => match exec::launch(&app.exec) {
                Ok(()) => {
                    self.hide_and_clear();
                    return;
                }
                Err(err) => Some(format!("could not launch {}: {err}", app.name)),
            },
            None => None,
        };
        self.state.borrow_mut().clear();
        search::clear(&self.entry);
        if let Some(message) = launch_error {
            eprintln!("launcher: {message}");
        }
    }

    /// Execute the highlighted row's plugin action, when it has one.
    /// `Copy` writes to the clipboard; `Open` launches `xdg-open`.
    fn plugin_action(&self) -> Result<(), String> {
        let action = self.state.borrow().selected_plugin_action().ok_or_else(|| "No plugin action".to_owned())?;
        match action {
            PluginAction::Copy { text } => {
                let _ = self.clipboard;
                self.window.clipboard().set_text(&text);
                Ok(())
            }
            PluginAction::Open { path } => {
                let status = std::process::Command::new("xdg-open")
                    .arg(&path)
                    .status()
                    .map_err(|err| format!("could not open {}: {err}", path))?;
                if status.success() {
                    Ok(())
                } else {
                    Err(format!("could not open {}: no default handler registered", path))
                }
            }
        }
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

    fn connect_clicks(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let list = self.results.list().clone();
        list.set_activate_on_single_click(false);
        let weak_selection = Rc::downgrade(self);
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return; };
            if !row.is_selectable() { return; }
            let row = row.clone();
            let weak_selection = weak_selection.clone();
            glib::idle_add_local_once(move || {
                let Some(window) = weak_selection.upgrade() else { return; };
                if let Some(index) = window.results.row_index(&row) {
                    window.state.borrow_mut().set_selection(Some(index));
                    window.results.highlight(Some(index));
                }
            });
        });
        list.connect_row_activated(move |_, row| {
            if !row.is_selectable() {
                return;
            }
            let Some(window) = weak.upgrade() else {
                return;
            };
            let index = window
                .results
                .row_index(row)
                .unwrap_or_else(|| window.state.borrow().selection().unwrap_or(0));
            window.select_or_activate(index);
        });
    }

    fn open_settings_window(&self) {
        let settings = gtk4::Window::builder()
            .title("Settings")
            .transient_for(&self.window)
            .modal(true)
            .default_width(420)
            .default_height(250)
            .resizable(false)
            .build();
        settings.add_css_class("launcher");
        // Theme switching is intentionally disabled until the existing live
        // theme application path is fixed; this phase stays dark by design.
        appearance::apply(&settings, &Appearance::default(), "dark");
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
        content.add_css_class("launcher-settings-surface");
        content.set_spacing(12);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);
        let retention = gtk4::ComboBoxText::new();
        for value in ClipboardRetention::ALL { retention.append(Some(value.as_toml()), value.as_label()); }
        let current = Config::load(&self.config_path).unwrap_or_default();
        retention.set_active_id(Some(current.clipboard.retention.as_toml()));
        let retention_label = gtk4::Label::new(Some("Clipboard retention"));
        retention_label.add_css_class("launcher-settings-label");
        content.append(&retention_label);
        content.append(&retention);
        let credit = gtk4::Label::new(Some("by Storm"));
        credit.add_css_class("launcher-settings-credit");
        credit.set_margin_top(12);
        content.append(&credit);
        let close = gtk4::Button::with_label("Close");
        close.add_css_class("launcher-settings-close");
        content.append(&close);
        settings.set_child(Some(&content));
        let settings_for_close = settings.clone();
        close.connect_clicked(move |_| settings_for_close.close());
        let path = self.config_path.clone();
        let history = self.history.clone();
        retention.connect_changed(move |retention| {
            if let Some(value) = retention.active_id() {
                let _ = Config::set_toml_string(&path, "clipboard", "retention", value.as_str());
                if let Some(retention) = ClipboardRetention::parse(value.as_str()) {
                    if let Ok(mut history) = history.lock() {
                        history.set_retention_seconds(retention.as_seconds());
                    }
                }
            }
        });
        settings.present();
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