//! Fuzzy search and relevance ranking over the application index.
//!
//! Search is delegated to the [`fuzzy-matcher`] crate, specifically its
//! Sublime-Text-style matcher ([`SkimMatcherV2`]). It was chosen over a
//! hand-written matcher because it already implements the heuristics a
//! launcher wants — prefix bonuses, word-boundary and camel-case bonuses,
//! exact-match and close-sequence scoring, case-insensitively — and it is
//! battle-tested at exactly this scale (it powers the `skim` TUI fuzzy
//! finder). Keeping it behind this module means the scoring policy and any
//! later ranking factors (launch frequency, categories, keywords, ...) can
//! evolve without touching the GTK layer.
//!
//! The module is deliberately widget-free: it maps a query and the application
//! index to a ranked list of `(index, score)` pairs. The caller owns the
//! application list and decides what to do with the order. Nothing here
//! mutates the index; an application is only ever read.
//!
//! Empty queries never reach the matcher: scoring every application against
//! an empty pattern is meaningless, so [`crate::launcher_state::LauncherState`]
//! falls back to the index's natural order instead of asking for a ranking.

use crate::index::AppEntry;
use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;

/// Any match scoring below this value is considered noise and dropped from
/// the results. Tuned against the [`SkimMatcherV2`] scale: genuine prefix,
/// word-boundary and close subsequence matches score well above it, while
/// accidental scattered matches — e.g. a single letter sitting mid-word with
/// no word-start occurrence (scores around 15) — land below it. A letter at
/// a word start scores higher again, so short queries still find real apps.
pub const MIN_MATCH_SCORE: i64 = 20;

/// One ranked result: which application matched, and how relevant it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoredMatch {
    /// Position into the application index.
    pub index: usize,
    /// Relevance score from the fuzzy matcher (higher is better).
    pub score: i64,
}

/// The application fuzzy matcher. A single instance is kept for the whole
/// session and reused across keystrokes, so the matcher holds no per-query
/// allocation between searches.
pub struct SearchRanker {
    matcher: SkimMatcherV2,
}

impl SearchRanker {
    /// Build a case-insensitive, Sublime-Text-style matcher.
    #[must_use]
    pub fn new() -> Self {
        Self {
            matcher: SkimMatcherV2::default().ignore_case(),
        }
    }

    /// Score one application against `query`.
    ///
    /// Returns `None` when the query does not occur as a character subsequence
    /// of the application name or desktop entry id. The higher score of the
    /// two fields wins, so both stay searchable. Exact uppercase letters in
    /// the query never change matching: everything is folded to lower case.
    #[must_use]
    pub fn score(&self, query: &str, app: &AppEntry) -> Option<i64> {
        match self.matcher.fuzzy_match(&app.name, query) {
            Some(score) => Some(match self.matcher.fuzzy_match(&app.id, query) {
                Some(id_score) => score.max(id_score),
                None => score,
            }),
            None => self.matcher.fuzzy_match(&app.id, query),
        }
    }

    /// Rank every application in `apps` against a non-empty `query`.
    ///
    /// Returns only matches at or above [`MIN_MATCH_SCORE`], sorted by
    /// descending relevance. Ties keep the original index order, so results
    /// are deterministic for a given index. The application list is never
    /// modified.
    #[must_use]
    pub fn rank(&self, query: &str, apps: &[AppEntry]) -> Vec<ScoredMatch> {
        let mut results = Vec::with_capacity(apps.len().min(64));
        for (index, app) in apps.iter().enumerate() {
            if let Some(score) = self.score(query, app) {
                if score >= MIN_MATCH_SCORE {
                    results.push(ScoredMatch { index, score });
                }
            }
        }
        results.sort_unstable_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
        results
    }
}

impl Default for SearchRanker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, id: &str) -> AppEntry {
        AppEntry {
            id: id.to_owned(),
            name: name.to_owned(),
            exec: format!("exec {name}"),
            icon: None,
        }
    }

    fn named<'a>(results: &[ScoredMatch], apps: &'a [AppEntry]) -> Vec<&'a str> {
        results.iter().map(|m| apps[m.index].name.as_str()).collect()
    }

    fn fixture() -> Vec<AppEntry> {
        vec![
            app("Firefox", "org.mozilla.firefox"),
            app("Firefox Developer Edition", "org.mozilla.firefoxdeveloperedition"),
            app("File Manager", "org.gnome.Nautilus"),
            app("GNOME Terminal", "org.gnome.Terminal"),
            app("Wireframe Editor", "org.example.wireframe"),
            app("Visual Studio Code", "com.microsoft.vscode"),
        ]
    }

    #[test]
    fn exact_match_ranks_first() {
        let apps = fixture();
        let results = named(&SearchRanker::new().rank("firefox", &apps), &apps);
        assert_eq!(results[0], "Firefox");
        assert!(
            results.iter().any(|name| *name == "Firefox Developer Edition"),
            "Firefox Developer Edition should still match 'firefox'"
        );
    }

    #[test]
    fn prefix_matches_outrank_mid_word_matches() {
        // "fire" is a prefix of "Firefox" but sits mid-word in "Wireframe
        // Editor" (f-i-r-e scattered across the second word). The prefix
        // match must come first.
        let apps = fixture();
        let results = named(&SearchRanker::new().rank("fire", &apps), &apps);
        assert_eq!(results[0], "Firefox");
        let wireframe = results.iter().position(|name| *name == "Wireframe Editor");
        assert!(
            wireframe.is_none_or(|position| position > 0),
            "a mid-word match is either dropped or ranked below the prefix match"
        );
    }

    #[test]
    fn close_sequences_outrank_scattered_ones() {
        // "code" is a contiguous bound word in "Visual Studio Code" but only a
        // sparse subsequence of "Cockatrice Online Deck Editor".
        let apps = vec![
            app("Cockatrice Online Deck Editor", "org.example.cockatrice"),
            app("Visual Studio Code", "com.microsoft.vscode"),
        ];
        let results = named(&SearchRanker::new().rank("code", &apps), &apps);
        assert_eq!(results[0], "Visual Studio Code");
    }

    #[test]
    fn word_boundary_matches_outrank_plain_substrings() {
        // "term" is the start of the word "Terminal" but only a substring of
        // "Patremium" (p-a-t-r-e-...). The word-boundary match wins.
        let apps = vec![
            app("Patremium Tasker", "org.example.patremium"),
            app("GNOME Terminal", "org.gnome.Terminal"),
        ];
        let results = named(&SearchRanker::new().rank("term", &apps), &apps);
        assert_eq!(results[0], "GNOME Terminal");
    }

    #[test]
    fn matching_is_case_insensitive() {
        let apps = fixture();
        for query in ["firefox", "Firefox", "FIREFOX", "fIrEfOx"] {
            let results = named(&SearchRanker::new().rank(query, &apps), &apps);
            assert_eq!(results[0], "Firefox", "query {query:?} should behave identically");
            assert!(
                results.iter().any(|name| *name == "Firefox Developer Edition"),
                "query {query:?} should still match Firefox Developer Edition"
            );
        }
    }

    #[test]
    fn applications_without_a_real_match_are_excluded() {
        let apps = fixture();
        let results = SearchRanker::new().rank("xyz123", &apps);
        assert!(results.is_empty(), "a query unrelated to every name must yield nothing");
        let results = SearchRanker::new().rank("zzzzzz", &apps);
        assert!(results.is_empty());
    }

    #[test]
    fn matching_only_needs_a_subsequence() {
        let apps = vec![
            app("Firefox", "org.mozilla.firefox"),
            app("Imaging Printer", "org.example.printer"),
        ];
        let results = named(&SearchRanker::new().rank("fire", &apps), &apps);
        assert_eq!(results[0], "Firefox");
    }

    #[test]
    fn many_results_are_sorted_by_descending_relevance() {
        let apps = vec![
            app("PyTerm", "org.example.pyterm"),
            app("Terminator", "org.example.terminator"),
            app("Terminal", "org.example.terminal"),
        ];
        let results = named(&SearchRanker::new().rank("term", &apps), &apps);
        assert_eq!(
            results,
            vec!["Terminator", "Terminal", "PyTerm"],
            "prefix matches rank above the word-embedded match; the two tied \
             prefix matches keep their index order"
        );
    }

    #[test]
    fn index_is_never_modified() {
        let apps = fixture();
        let snapshot = apps.clone();
        let ranker = SearchRanker::new();
        let _ = ranker.rank("fire", &apps);
        let _ = ranker.score("fire", &apps[0]);
        assert_eq!(apps, snapshot);
    }

    #[test]
    fn handles_unicode_names_without_panicking() {
        let apps = vec![
            app("融视频", "com.example.video"),
            app("Terminal", "org.gnome.Terminal"),
        ];
        let ranker = SearchRanker::new();
        // Unicode application names are searched safely: a multi-character
        // CJK query scores above the threshold and matches.
        let results = named(&ranker.rank("融视", &apps), &apps);
        assert_eq!(results, vec!["融视频"]);
        // A lone CJK character is a weak (sub-threshold) match; it is
        // filtered like any other weak character without panicking.
        assert!(ranker.rank("视", &apps).is_empty());
        // The same entry stays findable through its ASCII desktop entry id.
        let results = named(&ranker.rank("video", &apps), &apps);
        assert_eq!(results, vec!["融视频"]);
    }

    #[test]
    fn single_character_queries_are_safe() {
        let apps = fixture();
        let results = named(&SearchRanker::new().rank("f", &apps), &apps);
        assert!(results.contains(&"Firefox"), "a single word-start letter still matches");
        // Whitespace-only and heavily punctuated patterns must never panic or
        // produce bogus matches.
        for query in [" ", "a z 9?!", ""] {
            let _ = SearchRanker::new().rank(query, &apps);
        }
    }

    #[test]
    fn weak_spurious_matches_fall_below_the_threshold() {
        // "a" inside "Terminal" is a single mid-word letter with no word-start
        // 'a' anywhere in name or id: a genuinely weak match that must be
        // filtered out by MIN_MATCH_SCORE.
        let apps = vec![app("Terminal", "org.example.terminal")];
        assert!(SearchRanker::new().rank("a", &apps).is_empty());
        // The same letter at the start of a word clears the threshold.
        let apps = vec![app("Application 100", "org.example.app100")];
        assert!(!SearchRanker::new().rank("a", &apps).is_empty());
    }

    #[test]
    fn repeated_characters_and_spaces_are_safe() {
        let apps = fixture();
        let ranker = SearchRanker::new();
        // Repeated characters in a query still resolve to a real match.
        let results = named(&ranker.rank("ff", &apps), &apps);
        assert!(results.contains(&"Firefox"), "repeated 'f' should find Firefox");
        // Queries spanning spaces in a multi-word name work too.
        let results = named(&ranker.rank("firefox developer", &apps), &apps);
        assert_eq!(results[0], "Firefox Developer Edition");
        let results = named(&ranker.rank("file manager", &apps), &apps);
        assert_eq!(results[0], "File Manager");
    }
}