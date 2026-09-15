//! The results list: a vertical [`gtk4::ListBox`] rendered from the shared
//! [`LauncherState`]. Rows are kept in lock-step with the state's result
//! list, so highlighting one is a direct child lookup — no widget-tree
//! interpretation needed.
//!
//! A row is either an application ([`ListRow::App`], icon + name), a plugin
//! result ([`ListRow::Plugin`], title + optional subtitle) or a greyed-out
//! non-interactive hint ([`ListRow::Hint`], used when a plugin matched but
//! produced no rows). Plugin rows sit above application rows; hints behave
//! like them except they are not selectable and never activate.
//!
//! Rebuilding happens only when the *query* changes (a handful of rows), the
//! window itself and its search box are created once and reused.

use gtk4::prelude::*;
use launcher_core::index::AppEntry;
use launcher_core::launcher_state::{LauncherState, ListRow};
use launcher_plugins::PluginResult;
use std::cell::RefCell;
use std::rc::Rc;

/// Fallback icon used when a desktop entry has no `Icon` or the name cannot
/// be resolved (GTK already renders a neutral placeholder for unknown names;
/// this keeps the row well-formed in all cases).
const FALLBACK_ICON: &str = "application-x-executable";

/// Maximum scrollable height of the list. With ~40px rows this shows roughly
/// 10 results; more matches stay reachable by scrolling.
const MAX_RESULTS_HEIGHT: i32 = 400;

/// The results list and the widgets it currently shows.
pub struct ResultsView {
    scrolled: gtk4::ScrolledWindow,
    list: gtk4::ListBox,
    rows: RefCell<Vec<gtk4::ListBoxRow>>,
}

impl ResultsView {
    /// Build the results list (once; kept alive for the daemon's lifetime).
    #[must_use]
    pub fn new() -> Rc<Self> {
        let list = gtk4::ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::Single);
        list.set_activate_on_single_click(false);
        list.add_css_class("launcher-results");

        let scrolled = gtk4::ScrolledWindow::new();
        scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scrolled.set_max_content_height(MAX_RESULTS_HEIGHT);
        scrolled.set_vexpand(true);
        scrolled.set_child(Some(&list));

        Rc::new(Self {
            scrolled,
            list,
            rows: RefCell::new(Vec::new()),
        })
    }

    /// The widget to embed in the window.
    pub fn widget(&self) -> &gtk4::ScrolledWindow {
        &self.scrolled
    }

    /// Re-render the list from the current state. Called only when the query
    /// changes (or the window is (re)shown), never on every keystroke of an
    /// unchanged query.
    pub fn update(&self, state: &LauncherState) {
        for row in self.rows.borrow().iter() {
            self.list.remove(row);
        }
        self.rows.borrow_mut().clear();
        self.list.unselect_all();

        if state.results().is_empty() {
            self.list.append(&message_row(&format!("No results for \"{}\"", state.query())));
            return;
        }

        let apps = state.apps();
        for row in state.results() {
            let widget = match row {
                ListRow::App(position) => apps.get(*position).map(make_row),
                ListRow::Plugin { result, .. } => Some(make_plugin_row(result)),
                ListRow::Hint(text) => Some(hint_row(text)),
            };
            if let Some(widget) = widget {
                self.list.append(&widget);
                self.rows.borrow_mut().push(widget);
            }
        }
        self.highlight(state.selection());
    }

    /// Visually highlight the row at `position` (a position into the current
    /// result list). Removing the previous highlight and re-selecting keeps
    /// the `:selected`/`.selected` styles in sync. Rows that cannot be
    /// selected (hints) are skipped, so a highlight never lands on them.
    pub fn highlight(&self, position: Option<usize>) {
        self.list.unselect_all();
        let rows = self.rows.borrow();
        for (index, row) in rows.iter().enumerate() {
            let selected = Some(index) == position;
            if selected && row.is_selectable() {
                self.list.select_row(Some(row));
                row.add_css_class("selected");
            } else {
                row.remove_css_class("selected");
            }
        }
    }
}

/// A single selectable result row: icon on the left, application name on the
/// right.
fn make_row(app: &AppEntry) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.add_css_class("launcher-results-row");

    let icon = gtk4::Image::new();
    icon.add_css_class("launcher-result-icon");
    icon.set_pixel_size(24);
    icon.set_icon_name(Some(app.icon.as_deref().unwrap_or(FALLBACK_ICON)));

    let name = gtk4::Label::new(Some(&app.name));
    name.add_css_class("launcher-app-name");
    name.set_xalign(0.0);
    name.set_hexpand(true);

    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 14);
    hbox.append(&icon);
    hbox.append(&name);

    row.set_child(Some(&hbox));
    row
}

/// A single selectable plugin result row: title on top, optional subtitle
/// underneath. Plugins are text-centric — there is no icon; the title carries
/// the meaning (e.g. `echo: hello`).
fn make_plugin_row(result: &PluginResult) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.add_css_class("launcher-results-row");
    row.add_css_class("launcher-plugin-row");

    let title = gtk4::Label::new(Some(&result.title));
    title.add_css_class("launcher-app-name");
    title.set_xalign(0.0);
    title.set_hexpand(true);

    let lines = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
    lines.append(&title);
    if let Some(subtitle) = &result.subtitle {
        let subtitle = gtk4::Label::new(Some(subtitle));
        subtitle.add_css_class("launcher-result-subtitle");
        subtitle.set_xalign(0.0);
        subtitle.set_hexpand(true);
        lines.append(&subtitle);
    }

    row.set_child(Some(&lines));
    row
}

/// A greyed-out, non-selected row used for hints — guidance text shown in
/// place of an empty plugin result (e.g. an invalid calculator expression).
/// Unlike [`message_row`] it appears *inside* the result list, keeps its
/// position, and cannot be activated.
fn hint_row(text: &str) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.add_css_class("launcher-hint");
    row.set_selectable(false);

    let label = gtk4::Label::new(Some(text));
    label.add_css_class("launcher-hint-text");
    label.set_xalign(0.0);
    label.set_hexpand(true);
    label.set_single_line_mode(true);
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);

    row.set_child(Some(&label));
    row
}

/// A non-selectable row used for the empty state (no query matched at all).
fn message_row(text: &str) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.add_css_class("launcher-message");
    row.set_selectable(false);

    let label = gtk4::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_hexpand(true);
    row.set_child(Some(&label));
    row
}