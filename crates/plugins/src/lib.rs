//! Plugin system for the launcher.
//!
//! A plugin extends the launcher with domain-specific results (a calculator,
//! a clipboard manager, ...). Plugins are *prefix-triggered* by default: a
//! plugin can declare a trigger prefix such as `clip:`, and the launcher asks
//! it for results while the query starts with that prefix. Plugins may also
//! activate on other criteria — the [`calculator`] plugin, for one, activates
//! only when the query is a valid arithmetic expression.
//!
//! This crate defines the shared abstractions a plugin must implement:
//! [`Plugin`] (activation + results), [`PluginResult`] (one list row, with an
//! optional [`PluginAction`]) and [`PluginRegistry`] (the central collection
//! the daemon fills at startup). Built-ins live in [`echo`], [`calculator`]
//! and [`clipboard`]; a full plugin runtime and more concrete plugins are
//! added in later iterations.
//!
//! The crate deliberately stays GTK-free (as does `launcher-core`) so plugin
//! logic is testable without a display. GTK-only behaviour — rendering rows,
//! executing copy actions — is delegated to the UI crate. The [`clipboard`]
//! module also stays display-free: clipboard capture goes through the
//! [`clipboard::ClipboardSource`] trait so tests inject fakes.

pub mod calculator;
pub mod clipboard;
pub mod echo;
pub mod files;

/// An action the UI executes when the user activates (Enter) a result row.
///
/// Plugins stay GTK-free: they only declare *what* should happen and the GTK
/// layer performs it. `Copy` writes text to the system clipboard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginAction {
    /// Copy `text` verbatim to the system clipboard.
    Copy { text: String },
    /// Open `path` with the system default handler (`xdg-open`).
    Open { path: String },
}

/// A single entry produced by a plugin for a given query.
///
/// This is the row a plugin contributes to the launcher's result list. Apps
/// carry an `AppEntry` (icon + executable); plugins carry free-form text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginResult {
    /// Main line shown in the result list, e.g. `42`.
    pub title: String,
    /// Optional secondary line shown under the title.
    pub subtitle: Option<String>,
    /// Optional icon name for a row; used for file/folder results.
    pub icon_name: Option<String>,
    /// Optional label shown at the right edge of the row (e.g. `file`).
    pub tag: Option<String>,
    /// Optional action run when the row is activated (Enter). `None` for
    /// purely informational rows.
    pub action: Option<PluginAction>,
}

impl PluginResult {
    /// Create a result with only a title. No activation action.
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: None,
            icon_name: None,
            tag: None,
            action: None,
        }
    }

    /// Create a result with a title and a secondary line. No activation
    /// action.
    pub fn with_subtitle(title: impl Into<String>, subtitle: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            subtitle: Some(subtitle.into()),
            icon_name: None,
            tag: None,
            action: None,
        }
    }

    /// Attach an icon name to the row (e.g. `folder`, `text-x-generic`).
    #[must_use]
    pub fn with_icon(mut self, icon_name: impl Into<String>) -> Self {
        self.icon_name = Some(icon_name.into());
        self
    }

    /// Attach a right-aligned tag to the row (e.g. `file`).
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    /// Attach an action executed when the row is activated (Enter).
    #[must_use]
    pub fn with_action(mut self, action: PluginAction) -> Self {
        self.action = Some(action);
        self
    }
}

/// A plugin answers user queries with [`PluginResult`]s once it is active.
///
/// Activation is decided per query via [`Plugin::matches`]. The default
/// implementation is an ASCII-case-insensitive prefix check against the
/// plugin's [`Plugin::prefix`] — most plugins are "type the prefix, get the
/// plugin". A plugin that triggers differently overrides [`Plugin::matches`]
/// instead (the [`calculator`] plugin, for example, is active exactly while
/// the query is a valid arithmetic expression).
pub trait Plugin {
    /// Stable identifier, e.g. `echo`. Used by the `enabled_plugins` config
    /// allow-list.
    fn id(&self) -> &str;

    /// Human readable name, e.g. `Echo`.
    fn name(&self) -> &str;

    /// Optional trigger prefix, e.g. `calc:` or `clip:`. The default
    /// [`Plugin::matches`] reacts to it; plugins without a prefix must
    /// override [`Plugin::matches`].
    fn prefix(&self) -> Option<&str> {
        None
    }

    /// Whether this plugin should activate for `query`.
    ///
    /// The default implementation matches `query` against [`Plugin::prefix`]
    /// case-insensitively. Override for non-prefix activation rules.
    fn matches(&self, query: &str) -> bool {
        self.prefix().is_some_and(|prefix| has_prefix(query, prefix))
    }

    /// The results to display for `query`. Only called while
    /// [`Plugin::matches`] returned true.
    fn query(&self, query: &str) -> Vec<PluginResult>;

    /// Optional hint shown when the plugin matched but produced no rows.
    /// A plugin can use this for database-startup or empty-state guidance.
    fn empty_hint(&self, _query: &str) -> Option<String> {
        None
    }

    /// Whether this plugin has an async result still in flight for `query`.
    /// Default: plugins are synchronous and never pending.
    fn is_pending(&self, _query: &str) -> bool {
        false
    }
}

/// True when `query` starts with `prefix`, compared byte-wise and ignoring
/// ASCII case (so `ECHO: hi` triggers the `echo:` plugin).
///
/// The check is allocation-free and never panics on non-ASCII input: only the
/// first `prefix.len()` *bytes* are compared, so no string slicing happens.
#[must_use]
pub fn has_prefix(query: &str, prefix: &str) -> bool {
    query.len() >= prefix.len() && query.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// A type-erased, ordered collection of plugins.
///
/// The launcher fills one at startup ([`PluginRegistry::register`]) and
/// consults it for every query ([`PluginRegistry::dispatch`]). Registration
/// order decides which plugin wins when several claim the same query.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: Vec<Box<dyn Plugin>>,
}

impl PluginRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin. Plugins run in registration order; the first one
    /// that matches a query handles it.
    pub fn register(&mut self, plugin: Box<dyn Plugin>) {
        self.plugins.push(plugin);
    }

    /// The registered plugins, in registration order.
    pub fn plugins(&self) -> &[Box<dyn Plugin>] {
        &self.plugins
    }

    /// The number of registered plugins.
    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Whether no plugin is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// The plugin at `index`, if any.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&dyn Plugin> {
        self.plugins.get(index).map(Box::as_ref)
    }

    /// Drop every plugin for which `keep` returns false.
    ///
    /// The daemon uses this to apply the `enabled_plugins` config allow-list
    /// after registering the built-ins.
    pub fn retain(&mut self, mut keep: impl FnMut(&dyn Plugin) -> bool) {
        self.plugins.retain(|plugin| keep(plugin.as_ref()));
    }

    /// Activate the first plugin that matches `query` and collect its results.
    ///
    /// Returns `None` when no plugin wants the query, in which case the
    /// caller falls back to the ordinary application search. The caller
    /// decides how the plugin's results are combined with the application
    /// list; the launcher shows them first (see
    /// `launcher_core::launcher_state::LauncherState::refresh_results`).
    ///
    /// The returned `usize` is the plugin's position in the registry; result
    /// rows are tagged with it so an activation can later be routed back to
    /// the producing plugin.
    #[must_use]
    pub fn dispatch(&self, query: &str) -> Option<(usize, Vec<PluginResult>)> {
        for (index, plugin) in self.plugins.iter().enumerate() {
            if plugin.matches(query) {
                return Some((index, plugin.query(query)));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted fake plugin for exercising registry behaviour without a
    /// real domain plugin.
    struct FakePlugin {
        id: String,
        prefix: String,
    }

    impl FakePlugin {
        fn new(id: &str, prefix: &str) -> Self {
            Self {
                id: id.to_owned(),
                prefix: prefix.to_owned(),
            }
        }
    }

    impl Plugin for FakePlugin {
        fn id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            "Fake"
        }

        fn prefix(&self) -> Option<&str> {
            Some(&self.prefix)
        }

        fn query(&self, query: &str) -> Vec<PluginResult> {
            vec![PluginResult::new(format!("{} got {query}", self.id))]
        }
    }

    #[test]
    fn has_prefix_checks_ascii_case_insensitively() {
        assert!(has_prefix("echo: hello", "echo:"));
        assert!(has_prefix("ECHO: hello", "echo:"));
        assert!(has_prefix("Calc: 2+2", "calc:"));
        assert!(!has_prefix("ech", "echo:"), "query shorter than prefix");
        assert!(!has_prefix(" echo: hello", "echo:"), "must be a true prefix");
        assert!(!has_prefix("firefox", "echo:"));
    }

    #[test]
    fn has_prefix_never_panics_on_unicode() {
        assert!(!has_prefix("¿echo: hola?", "echo:"));
        assert!(has_prefix("echo: ñoño", "echo:"));
    }

    #[test]
    fn default_matches_is_a_prefix_check() {
        let plugin = FakePlugin::new("fake", "calc:");
        assert!(plugin.matches("calc: 2+2"));
        assert!(plugin.matches("CALC: 2+2"));
        assert!(!plugin.matches("calculator"));
    }

    #[test]
    fn dispatch_returns_the_results_of_the_matching_plugin() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(FakePlugin::new("calc", "calc:")));
        registry.register(Box::new(FakePlugin::new("clip", "clip:")));

        let (index, results) = registry.dispatch("clip: primary").expect("clip plugin");
        assert_eq!(index, 1);
        assert_eq!(results, vec![PluginResult::new("clip got clip: primary")]);
    }

    #[test]
    fn dispatch_prefers_registration_order() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(FakePlugin::new("first", "work:")));
        registry.register(Box::new(FakePlugin::new("second", "work:")));

        let (index, results) = registry.dispatch("work: x").expect("a plugin");
        assert_eq!(index, 0, "first registered plugin wins");
        assert_eq!(results[0].title, "first got work: x");
    }

    #[test]
    fn dispatch_returns_none_without_a_matching_plugin() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(FakePlugin::new("calc", "calc:")));
        assert_eq!(registry.dispatch("firefox"), None);
        assert_eq!(registry.dispatch("calc"), None, "prefix needs the colon");
    }

    #[test]
    fn dispatch_on_an_empty_registry_is_none() {
        let registry = PluginRegistry::new();
        assert_eq!(registry.dispatch("anything"), None);
        assert!(registry.is_empty());
    }

    #[test]
    fn retain_filters_by_plugin_id() {
        let mut registry = PluginRegistry::new();
        registry.register(Box::new(FakePlugin::new("echo", "echo:")));
        registry.register(Box::new(FakePlugin::new("calc", "calc:")));

        let enabled = ["echo"];
        registry.retain(|plugin| enabled.contains(&plugin.id()));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.get(0).map(|p| p.id()), Some("echo"));
        assert_eq!(registry.dispatch("calc: 1+1"), None);
        assert_ne!(registry.dispatch("echo: hi"), None);
    }

    #[test]
    fn results_default_to_no_action_and_builders_attach_it() {
        assert_eq!(PluginResult::new("x").action, None);
        assert_eq!(PluginResult::with_subtitle("x", "y").action, None);

        let copy = PluginResult::with_subtitle("x", "y")
            .with_action(PluginAction::Copy { text: "1".into() });
        assert_eq!(copy.action, Some(PluginAction::Copy { text: "1".into() }));
        assert_eq!(copy.subtitle.as_deref(), Some("y"), "with_action preserves the rest");
    }

    #[test]
    fn plugins_can_supply_a_custom_empty_hint() {
        struct HintPlugin;
        impl Plugin for HintPlugin {
            fn id(&self) -> &str {
                "hint"
            }
            fn name(&self) -> &str {
                "Hint"
            }
            fn prefix(&self) -> Option<&str> {
                Some("f:")
            }
            fn query(&self, _query: &str) -> Vec<PluginResult> {
                Vec::new()
            }
            fn empty_hint(&self, _query: &str) -> Option<String> {
                Some("The file index is still being built…".to_owned())
            }
        }

        let plugin = HintPlugin;
        assert_eq!(plugin.empty_hint("f:foo"), Some("The file index is still being built…".to_owned()));
    }

    #[test]
    fn result_rows_can_carry_icon_and_tag_metadata() {
        let result = PluginResult::new("README.md")
            .with_icon("text-x-generic")
            .with_tag("file");
        assert_eq!(result.icon_name.as_deref(), Some("text-x-generic"));
        assert_eq!(result.tag.as_deref(), Some("file"));
    }
}