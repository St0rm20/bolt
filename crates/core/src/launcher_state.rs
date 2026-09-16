//! Pure, GTK-free launcher state: the in-memory application index, the
//! registered plugins, the current search query, the ranked results and the
//! highlighted row.
//!
//! The UI crate renders this state; everything that decides *what* to show
//! lives here so it is testable without a display. Application matching is
//! delegated to the fuzzy search module ([`crate::search`]) — a
//! Sublime-Text-style matcher that ranks applications by relevance. Plugins
//! are delegated to a [`PluginRegistry`]; while one matches the query its
//! results come *first*, followed by up to ten ranked applications (see
//! [`LauncherState::refresh_results`]). Only result *positions* and plugin
//! rows are stored; nothing is rebuilt while typing.

use crate::index::AppEntry;
use crate::search::SearchRanker;
use launcher_plugins::{Plugin, PluginAction, PluginResult, PluginRegistry};

/// A single row in the launcher's result list: either an application (a
/// position into [`LauncherState::apps`]) or a plugin-provided result.
///
/// Plugin results always appear above application rows; both kinds can be
/// present at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow {
    /// An application at the given position in the app index.
    App(usize),
    /// A plugin result, tagged with the plugin's position in the registry so
    /// the row can later be routed back to the plugin that produced it.
    Plugin {
        plugin: usize,
        result: PluginResult,
    },
    /// A non-activatable hint (a plugin that matched but produced no rows).
    /// Rendered grey in the UI; never usable with Enter.
    Hint(String),
}

impl ListRow {
    /// The application position, when this row is an application.
    #[must_use]
    pub fn as_app(&self) -> Option<usize> {
        match self {
            Self::App(position) => Some(*position),
            Self::Plugin { .. } | Self::Hint(_) => None,
        }
    }

    /// The (plugin position, result) pair, when this row is a plugin result.
    #[must_use]
    pub fn as_plugin(&self) -> Option<(usize, &PluginResult)> {
        match self {
            Self::App(_) | Self::Hint(_) => None,
            Self::Plugin { plugin, result } => Some((*plugin, result)),
        }
    }

    /// The hint text, when this row is a hint.
    #[must_use]
    pub fn as_hint(&self) -> Option<&str> {
        match self {
            Self::Hint(text) => Some(text),
            Self::App(_) | Self::Plugin { .. } => None,
        }
    }
}

/// Mutable application state shared with the GTK layer.
pub struct LauncherState {
    /// The in-memory index, built once by the daemon and never rebuilt while
    /// the user is typing.
    apps: Vec<AppEntry>,
    /// Registered plugins, consulted for every query.
    plugins: PluginRegistry,
    /// The fuzzy matcher, reused for every keystroke.
    ranker: SearchRanker,
    /// Current search query.
    query: String,
    /// The rows currently shown, most relevant first: plugin results while an
    /// active plugin matches the query, ranked applications otherwise (and up
    /// to [`APP_ROWS_BELOW_PLUGIN`] of them below a plugin's rows).
    results: Vec<ListRow>,
    /// Position (into `results`) currently highlighted, if any.
    selection: Option<usize>,
    /// Mirrors whether the launcher window is shown right now.
    visible: bool,
}

impl LauncherState {
    /// Build fresh state over an application index. The index is taken over:
    /// the caller must hand the same `Vec` to the UI and then discard it,
    /// because the search data is derived from it here. No plugins are
    /// registered.
    pub fn new(apps: Vec<AppEntry>) -> Self {
        Self::with_plugins(apps, PluginRegistry::new())
    }

    /// Build fresh state over an application index and a plugin registry. The
    /// index is taken over as in [`Self::new`].
    pub fn with_plugins(apps: Vec<AppEntry>, plugins: PluginRegistry) -> Self {
        let mut state = Self {
            apps,
            plugins,
            ranker: SearchRanker::new(),
            query: String::new(),
            results: Vec::new(),
            selection: None,
            visible: false,
        };
        state.refresh_results();
        state
    }

    /// The full application index.
    pub fn apps(&self) -> &[AppEntry] {
        &self.apps
    }

    /// The current search query.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The rows currently shown, in display order.
    pub fn results(&self) -> &[ListRow] {
        &self.results
    }

    /// Position (into [`Self::results`]) of the highlighted row, if any.
    pub fn selection(&self) -> Option<usize> {
        self.selection
    }

    /// The highlighted [`AppEntry`], when the highlighted row is an
    /// application.
    pub fn selected_app(&self) -> Option<&AppEntry> {
        match self.selection.and_then(|position| self.results.get(position)) {
            Some(ListRow::App(position)) => self.apps.get(*position),
            Some(ListRow::Plugin { .. }) | Some(ListRow::Hint(_)) | None => None,
        }
    }

    /// The plugin and its result behind the highlighted row, when it is a
    /// plugin result.
    pub fn selected_plugin(&self) -> Option<(&dyn Plugin, &PluginResult)> {
        let (plugin, result) = self
            .selection
            .and_then(|position| self.results.get(position))
            .and_then(ListRow::as_plugin)?;
        Some((self.plugins.get(plugin)?, result))
    }

    /// The activation action of the highlighted row, when it is a plugin
    /// result that carries one. Returns `None` for app rows, other rows
    /// without actions, and when nothing is highlighted.
    #[must_use]
    pub fn selected_plugin_action(&self) -> Option<PluginAction> {
        self.selection
            .and_then(|position| self.results.get(position))
            .and_then(ListRow::as_plugin)
            .and_then(|(_, result)| result.action.clone())
    }
    /// Whether the launcher window is currently visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Mark the launcher window visible or hidden (mirrored by the UI).
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Replace the search query and recompute the ranked results.
    /// Highlighting resets to the first (most relevant) match.
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.refresh_results();
    }

    /// Clear the query; the whole index becomes the result list again.
    pub fn clear(&mut self) {
        self.set_query(String::new());
    }

    /// Move the highlight by `delta` (−1 / +1) within the current results,
    /// wrapping around the edges.
    pub fn move_selection(&mut self, delta: isize) {
        self.selection = move_position(self.selection, self.results.len(), delta);
    }

    /// Directly set the highlighted row. Used by mouse clicks so the pointer's
    /// selection and the keyboard's `selected_index` are one and the same
    /// [`Self::selection`] — there is no separate pointer-selection state.
    /// Positions beyond the current result list clamp to `None`.
    pub fn set_selection(&mut self, position: impl Into<Option<usize>>) {
        self.selection = position.into().filter(|position| *position < self.results.len());
    }

    /// Recompute the ranked result list for the current query.
    ///
    /// The empty query short-circuits to the index's natural order without
    /// running the fuzzy matcher — that keeps the initial launcher state
    /// cheap and predictable.
    ///
    /// For a non-empty query the first plugin that matches ([`PluginRegistry::dispatch`])
    /// contributes its rows *first*; the top [`APP_ROWS_BELOW_PLUGIN`]
    /// application matches are then shown beneath them. This keeps a prefix
    /// plugin like `echo:` present (better than letting the results
    /// disappear on the app side) and lets a triggerless plugin like the
    /// calculator sit above the application list. A matching plugin that
    /// yields no rows (an invalid calculator expression, for instance) shows
    /// a single greyed-out [`ListRow::Hint`] instead, so the query never
    /// answers the user with silence. Without a matching plugin the list is
    /// the plain ranking.
    fn refresh_results(&mut self) {
        let query = self.query.trim();
        self.results = if query.is_empty() {
            (0..self.apps.len()).map(ListRow::App).collect()
        } else if let Some((plugin, rows)) = self.plugins.dispatch(query) {
            let mut combined = if rows.is_empty() {
                let plugin_name = self.plugins.get(plugin).map_or("?plugin?", |p| p.name());
                vec![ListRow::Hint(no_results_hint(plugin_name))]
            } else {
                rows.into_iter()
                    .map(|result| ListRow::Plugin { plugin, result })
                    .collect()
            };
            combined.extend(
                self.ranker
                    .rank(query, &self.apps)
                    .into_iter()
                    .take(APP_ROWS_BELOW_PLUGIN)
                    .map(|matched| ListRow::App(matched.index)),
            );
            combined
        } else {
            self.ranker
                .rank(query, &self.apps)
                .into_iter()
                .map(|matched| ListRow::App(matched.index))
                .collect()
        };
        // Guarantee the "at most one Hint row" invariant: only a matching
        // plugin can produce a hint today, and `dispatch` yields a single
        // plugin, so this filters defensively (a future plugin API could not
        // regress the UI into stacked hint rows).
        let mut saw_hint = false;
        self.results.retain(|row| match row {
            ListRow::Hint(_) if saw_hint => false,
            ListRow::Hint(_) => {
                saw_hint = true;
                true
            }
            _ => true,
        });
        self.selection = if self.results.is_empty() { None } else { Some(0) };
    }
}

/// Maximum number of application rows shown beneath a plugin's rows.
///
/// Fuzzy-rank relevance is meaningless next to a plugin result ("echo:" and
/// "27 * 3" score nothing against the app list), so a small fixed slice keeps
/// the plugin visible while offering a hint of what's an application.
pub const APP_ROWS_BELOW_PLUGIN: usize = 10;

/// Text for a hint row shown when a plugin matched the query but produced no
/// results — e.g. an invalid calculator expression. Ghosted in the UI so it
/// reads as guidance, not as an actionable row.
fn no_results_hint(plugin_name: &str) -> String {
    format!("No results — try another query (plugin: {plugin_name})")
}

/// Move a selection within `count` items by `delta`, wrapping around the edges.
///
/// Returns `None` when there is nothing to select (`count == 0`). A `None`
/// current position selects the first (or last for negative deltas) item.
pub fn move_position(position: Option<usize>, count: usize, delta: isize) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let current = position.unwrap_or(0) as isize;
    let next = (current + delta).rem_euclid(count as isize) as usize;
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use launcher_plugins::echo::EchoPlugin;

    fn app(name: impl Into<String>, id: impl Into<String>) -> AppEntry {
        let name = name.into();
        AppEntry {
            id: id.into(),
            exec: format!("exec {name}"),
            name,
            icon: None,
        }
    }

    fn fixture() -> Vec<AppEntry> {
        vec![
            app("Firefox", "org.mozilla.firefox"),
            app("GNOME Terminal", "org.gnome.Terminal"),
            app("Visual Studio Code", "com.microsoft.vscode"),
            app("Firefox Developer Edition", "org.mozilla.firefoxdeveloperedition"),
        ]
    }

    #[test]
    fn empty_query_selects_everything_and_first_entry() {
        let state = LauncherState::new(fixture());
        assert_eq!(app_indexes(&state), [0, 1, 2, 3]);
        assert_eq!(state.selection(), Some(0));
        assert_eq!(state.selected_app().map(|a| a.name.as_str()), Some("Firefox"));
    }

    #[test]
    fn fuzzy_matching_ranks_by_relevance() {
        let mut state = LauncherState::new(fixture());
        state_equals(&mut state, "fire", &["Firefox", "Firefox Developer Edition"]);
        state_equals(&mut state, "terminal", &["GNOME Terminal"]);
        state_equals(&mut state, "code", &["Visual Studio Code"]);
        state_equals(&mut state, "TERM", &["GNOME Terminal"]);
    }

    #[test]
    fn matches_against_the_desktop_entry_id_too() {
        let mut state = LauncherState::new(fixture());
        // "mozi" does not appear in any display name but both Firefox desktop
        // entry ids contain it, so both rank via the id.
        state_equals(&mut state, "mozi", &["Firefox", "Firefox Developer Edition"]);
    }

    #[test]
    fn no_matches_yield_empty_results_and_no_selection() {
        let mut state = LauncherState::new(fixture());
        state_equals(&mut state, "zzzzzz", &[]);
        assert_eq!(state.selection(), None);
        assert_eq!(state.selected_app(), None);
    }

    #[test]
    fn clearing_the_query_restores_the_full_list() {
        let mut state = LauncherState::new(fixture());
        state.set_query("fire");
        assert_eq!(state.results().len(), 2);
        state.clear();
        assert_eq!(state.results().len(), 4);
        assert_eq!(state.selection(), Some(0));
        assert_eq!(state.query(), "");
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut state = LauncherState::new(fixture());
        state.set_query("fire");
        // ["Firefox", "Firefox Developer Edition"]
        assert_eq!(state.selection(), Some(0));
        state.move_selection(1);
        assert_eq!(state.selection(), Some(1));
        state.move_selection(1);
        assert_eq!(state.selection(), Some(0), "down past the last result wraps");
        state.move_selection(-1);
        assert_eq!(state.selection(), Some(1), "up past the first result wraps");
    }

    #[test]
    fn selection_clamps_when_only_one_result() {
        let mut state = LauncherState::new(fixture());
        state.set_query("terminal");
        assert_eq!(state.results().len(), 1);
        state.move_selection(1);
        assert_eq!(state.selection(), Some(0));
        state.move_selection(-1);
        assert_eq!(state.selection(), Some(0));
    }

    #[test]
    fn move_position_behaves_on_empty_and_boundaries() {
        assert_eq!(move_position(None, 0, 1), None);
        assert_eq!(move_position(Some(2), 0, 1), None);
        assert_eq!(move_position(Some(0), 1, 1), Some(0));
        assert_eq!(move_position(Some(2), 3, 1), Some(0));
        assert_eq!(move_position(Some(0), 3, -1), Some(2));
        assert_eq!(move_position(Some(2), 3, 0), Some(2));
        assert_eq!(move_position(None, 3, 1), Some(1), "None starts at the first result");
    }

    #[test]
    fn typing_does_not_mutate_the_shared_index() {
        let mut state = LauncherState::new(fixture());
        let snapshot = state.apps().to_vec();
        state.set_query("fire");
        state.move_selection(1);
        state.clear();
        assert_eq!(state.apps(), &snapshot[..], "the index must never be rebuilt or moved");
    }

    #[test]
    fn handles_thousands_of_apps_quickly() {
        let apps: Vec<AppEntry> = (0..5000)
            .map(|i| app(format!("Application {i}"), format!("org.example.app{i}")))
            .collect();
        let mut state = LauncherState::new(apps);
        assert_eq!(state.results().len(), 5000);
        state.set_query("4999");
        assert_eq!(app_indexes(&state), [4999]);
        state.move_selection(1);
        assert_eq!(state.selection(), Some(0));
    }

    #[test]
    fn selection_stays_valid_after_query_changes() {
        let mut state = LauncherState::new(fixture());
        // A non-empty query resets the highlight to the top result, so it can
        // never point at a result that was filtered out.
        state.set_query("fire");
        state.move_selection(1);
        state.set_query("terminal");
        assert_eq!(state.selection(), Some(0));
        assert_eq!(state.selected_app().map(|a| a.name.as_str()), Some("GNOME Terminal"));
        state.set_query("zzzz");
        assert_eq!(state.selection(), None);
        assert_eq!(state.selected_app(), None);
    }

    #[test]
    fn visibility_flag_is_mirrored() {
        let mut state = LauncherState::new(fixture());
        assert!(!state.is_visible());
        state.set_visible(true);
        assert!(state.is_visible());
        state.set_visible(false);
        assert!(!state.is_visible());
    }

    /// Assert that a query yields exactly the apps with these display names,
    /// in rank order. App rows are compared regardless of plugin rows above
    /// them.
    fn state_equals(state: &mut LauncherState, query: &str, expected: &[&str]) {
        state.set_query(query);
        let names: Vec<String> = state
            .results()
            .iter()
            .filter_map(ListRow::as_app)
            .map(|position| state.apps()[position].name.clone())
            .collect();
        let expected: Vec<String> = expected.iter().map(|name| name.to_string()).collect();
        assert_eq!(names, expected, "query: {query:?}");
    }

    /// The application positions behind every row currently shown (plugin
    /// rows are ignored).
    fn app_indexes(state: &LauncherState) -> Vec<usize> {
        state.results().iter().filter_map(ListRow::as_app).collect()
    }

    /// State over [`fixture`] with the built-in echo plugin registered.
    fn plugin_state() -> LauncherState {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(EchoPlugin));
        LauncherState::with_plugins(fixture(), registry)
    }

    /// A fake plugin that matches every query and yields a single fixed row:
    /// stands in for a triggerless plugin like the calculator.
    struct AlwaysMatchPlugin;

    impl Plugin for AlwaysMatchPlugin {
        fn id(&self) -> &str {
            "always"
        }

        fn name(&self) -> &str {
            "Always"
        }

        fn matches(&self, _query: &str) -> bool {
            true
        }

        fn query(&self, _query: &str) -> Vec<PluginResult> {
            vec![PluginResult::with_subtitle("fixed", "Always")]
        }
    }

    /// State over [`fixture`] with an always-matching plugin registered.
    fn always_match_state() -> LauncherState {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(AlwaysMatchPlugin));
        LauncherState::with_plugins(fixture(), registry)
    }

    #[test]
    fn prefix_query_activates_the_matching_plugin() {
        let mut state = plugin_state();
        state.set_query("echo: hello");
        let (plugin, result) = state.selected_plugin().expect("a plugin row is selected");
        assert_eq!(plugin.id(), "echo");
        assert_eq!(result.title, "echo: hello");
        // "echo: hello" is not a real app query, so only the plugin row shows.
        assert_eq!(state.results().len(), 1);
    }

    #[test]
    fn plugin_prefix_matching_is_case_insensitive() {
        let mut state = plugin_state();
        state.set_query("ECHO: hello");
        let (plugin, result) = state.selected_plugin().expect("a plugin row is selected");
        assert_eq!(plugin.id(), "echo");
        assert_eq!(result.title, "echo: hello");
    }

    #[test]
    fn non_prefix_queries_skip_plugins_and_rank_apps() {
        let mut state = plugin_state();
        state.set_query("fire");
        assert_eq!(app_indexes(&state), [0, 3]);
        assert!(state.selected_plugin().is_none());
    }

    #[test]
    fn empty_query_shows_apps_even_with_plugins_registered() {
        let state = plugin_state();
        assert_eq!(app_indexes(&state), [0, 1, 2, 3]);
        assert!(state.selected_plugin().is_none());
    }

    #[test]
    fn selection_navigates_plugin_results() {
        let mut state = plugin_state();
        state.set_query("echo: a");
        assert_eq!(state.selection(), Some(0));
        assert!(state.selected_plugin().is_some());
        state.move_selection(1);
        assert_eq!(state.selection(), Some(0), "a single result wraps around");
    }

    #[test]
    fn plugin_rows_survive_clearing_the_query() {
        let mut state = plugin_state();
        state.set_query("echo: hello");
        assert!(state.selected_plugin().is_some());
        state.clear();
        assert_eq!(app_indexes(&state), [0, 1, 2, 3]);
        assert!(state.selected_plugin().is_none());
    }

    #[test]
    fn plugin_rows_come_first_and_apps_ranked_below() {
        let mut state = always_match_state();
        state.set_query("fire");
        let rows = state.results();
        assert_eq!(rows[0], ListRow::Plugin { plugin: 0, result: PluginResult::with_subtitle("fixed", "Always") });
        assert_eq!(app_indexes(&state), [0, 3], "ranked apps follow the plugin row");
    }

    #[test]
    fn app_rows_below_a_plugin_are_capped() {
        // 12 of the 5000 fake apps match "Application 1?"; the plugin row
        // still shows first and only the top ten apps follow.
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(AlwaysMatchPlugin));
        let apps: Vec<AppEntry> = (0..12)
            .map(|i| app(format!("Application {i}"), format!("org.example.app{i}")))
            .collect();
        let mut state = LauncherState::with_plugins(apps, registry);
        state.set_query("application");
        assert_eq!(state.results().len(), 1 + APP_ROWS_BELOW_PLUGIN);
        assert!(matches!(state.results()[0], ListRow::Plugin { .. }));
    }

    #[test]
    fn active_plugin_without_results_shows_a_hint_row() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(AlwaysMatchEmptyPlugin));
        let mut state = LauncherState::with_plugins(fixture(), registry);
        state.set_query("2 +");
        assert_eq!(state.results().len(), 1, "a hint row instead of silence");
        let hint = state.results()[0].as_hint().expect("the row is a hint");
        assert!(hint.contains("try another query"), "hint text: {hint:?}");
        assert_eq!(state.selected_app(), None, "hint rows are never applications");
        assert!(state.selected_plugin().is_none(), "hint rows have no plugin behind them");
    }

    struct AlwaysMatchEmptyPlugin;

    impl Plugin for AlwaysMatchEmptyPlugin {
        fn id(&self) -> &str {
            "always-empty"
        }

        fn name(&self) -> &str {
            "Always Empty"
        }

        fn matches(&self, _query: &str) -> bool {
            true
        }

        fn query(&self, _query: &str) -> Vec<PluginResult> {
            Vec::new()
        }
    }

    #[test]
    fn selected_plugin_action_reports_the_configured_action() {
        use launcher_plugins::{calculator::CalculatorPlugin, PluginAction};
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(CalculatorPlugin));
        let mut state = LauncherState::with_plugins(fixture(), registry);
        state.set_query("2 + 2");
        assert_eq!(
            state.selected_plugin_action(),
            Some(PluginAction::Copy { text: "4".into() })
        );
        state.set_query("firefox");
        assert_eq!(state.selected_plugin_action(), None, "app rows have no plugin action");
    }

    /// State over [`fixture`] with only the built-in calculator registered.
    fn calculator_state() -> LauncherState {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(launcher_plugins::calculator::CalculatorPlugin));
        LauncherState::with_plugins(fixture(), registry)
    }

    /// The number of hint rows currently shown.
    fn hint_count(state: &LauncherState) -> usize {
        state.results().iter().filter(|row| row.as_hint().is_some()).count()
    }

    #[test]
    fn incomplete_expression_shows_exactly_one_hint_which_is_replaced() {
        // Regression: hint rows must never stack. `6/` is arithmetic-looking
        // but not yet evaluable, so the calculator matches and yields a single
        // hint — and nothing else.
        let mut state = calculator_state();
        state.set_query("6/");
        assert_eq!(state.results().len(), 1, "a single hint, not a stack");
        assert_eq!(hint_count(&state), 1);
        assert_eq!(state.selection(), Some(0));

        // Completing the expression replaces the hint with the result row.
        state.set_query("6/2");
        assert_eq!(hint_count(&state), 0, "the hint disappears the instant the query completes");
        assert!(matches!(state.results().first(), Some(ListRow::Plugin { .. })));
        let (_, result) = state.results()[0].as_plugin().expect("a plugin row");
        assert_eq!(result.title, "3", "6/2 is ordinary division");

        // Clearing the field resets to the full, unfiltered app list.
        state.clear();
        assert_eq!(app_indexes(&state), [0, 1, 2, 3]);
        assert_eq!(hint_count(&state), 0, "no leftover hint after clearing");
    }

    #[test]
    fn hint_disappears_when_the_query_stops_matching() {
        let mut state = calculator_state();
        state.set_query("6/");
        assert_eq!(hint_count(&state), 1);
        // A wordy query stops the calculator (and matches an app instead).
        state.set_query("fire");
        assert_eq!(hint_count(&state), 0);
        assert_eq!(app_indexes(&state), [0, 3]);
        // An empty query resets too.
        state.set_query("6/");
        state.clear();
        assert_eq!(hint_count(&state), 0);
    }

    #[test]
    fn hint_rows_are_never_actionable() {
        let mut state = calculator_state();
        state.set_query("6/");
        assert!(state.selected_plugin_action().is_none(), "a hint has no activation action");
        assert!(state.selected_app().is_none(), "a hint is never an application");
        assert!(state.selected_plugin().is_none(), "a hint has no plugin behind it");
    }

    #[test]
    fn set_selection_replaces_the_keyboard_selection() {
        let mut state = LauncherState::new(fixture());
        state.set_query("fire");
        assert_eq!(state.selection(), Some(0));
        // Mouse click on the second "fire" match: shares the same selected_index.
        state.set_selection(Some(1));
        assert_eq!(state.selection(), Some(1));
        assert_eq!(state.selected_app().map(|a| a.name.as_str()), Some("Firefox Developer Edition"));
        // Arrow keys continue from where the mouse left off.
        state.move_selection(1);
        assert_eq!(state.selection(), Some(0), "wraps from the mouse position");
        state.set_selection(Some(99));
        assert_eq!(state.selection(), None, "out-of-range selections clamp to none");
    }

    #[test]
    fn calculator_results_rank_above_application_matches() {
        use launcher_plugins::calculator::CalculatorPlugin;
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(CalculatorPlugin));
        let mut state = LauncherState::with_plugins(fixture(), registry);
        // "2 + 2" also fuzzy-matches nothing here, so the single calculator
        // row is both first and selected.
        state.set_query("2 + 2");
        let (plugin, result) = state.selected_plugin().expect("calculator row selected");
        assert_eq!(plugin.id(), "calculator");
        assert_eq!(result.title, "4");
        assert!(state.selected_app().is_none());
    }
}