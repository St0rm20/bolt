//! The launcher's search box — a [`gtk4::SearchEntry`] wired to report every
//! text change. Widget construction is grouped here so the window code stays
//! about composition, not GtkEntry plumbing.

use gtk4::prelude::*;

/// Build the launcher search box.
///
/// The returned entry emits `search-changed` on every keystroke (and when the
/// text is cleared), so consumers simply subscribe with [`connect_changed`].
///
/// [`connect_changed`]: Self::connect_changed
#[must_use]
pub fn new() -> gtk4::SearchEntry {
    let entry = gtk4::SearchEntry::new();
    entry.set_placeholder_text(Some("Type to search…"));
    entry.set_hexpand(true);
    entry.add_css_class("launcher-search");
    entry
}

/// Subscribe to search text changes. The callback runs on the GTK main thread
/// with the current (trimmed) text.
pub fn connect_changed(entry: &gtk4::SearchEntry, on_change: impl Fn(&str) + 'static) {
    entry.connect_search_changed(move |entry| {
        on_change(entry.text().as_str());
    });
}

/// Clear the search box. Because clearing is itself a text change, any
/// [`connect_changed`] subscribers fire and the results reset naturally.
pub fn clear(entry: &gtk4::SearchEntry) {
    use gtk4::prelude::EditableExt;
    entry.set_text("");
}