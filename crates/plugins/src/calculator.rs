//! `calculator` — evaluates arithmetic expressions typed into the search box.
//!
//! The calculator activates *without a prefix*: [`CalculatorPlugin::matches`]
//! delegates to the expression evaluator, so the plugin lights up exactly
//! while the query is a valid expression (`2 + 2`) and stays silent for
//! ordinary searches (`firefox`). When active its single result — the
//! formatted value — is shown above any application matches. Activating the
//! row (Enter) copies the value to the clipboard via a
//! [`PluginAction::Copy`].
//!
//! Expressions are parsed and evaluated with the `meval` crate (no shell, no
//! dynamic execution). A bare context — no constants, no function calls —
//! deliberately exposes only the arithmetic operators `+ - * / ^` and
//! parentheses; everything else (identifiers, stray symbols, incomplete
//! input) simply fails to evaluate and never activates the plugin.

use crate::{Plugin, PluginAction, PluginResult};
use meval::{Context, Expr};

/// Identifier used by the `enabled_plugins` config allow-list.
pub const CALCULATOR_PLUGIN_ID: &str = "calculator";

/// The calculator plugin.
#[derive(Debug, Clone, Copy, Default)]
pub struct CalculatorPlugin;

/// Maximum number of decimal digits shown for non-integral results.
const MAX_DECIMALS: usize = 10;

impl Plugin for CalculatorPlugin {
    fn id(&self) -> &str {
        CALCULATOR_PLUGIN_ID
    }

    fn name(&self) -> &str {
        "Calculator"
    }

    /// No prefix: activation follows expression validity.
    fn matches(&self, query: &str) -> bool {
        finite_value(query).is_some()
    }

    fn query(&self, query: &str) -> Vec<PluginResult> {
        result_for(query).into_iter().collect()
    }
}

/// Build the result for `query` when it is a valid, finite expression: the
/// formatted value as title, `Calculator` as subtitle, and a copy action so
/// Enter puts the value on the clipboard.
fn result_for(query: &str) -> Option<PluginResult> {
    let value = finite_value(query)?;
    let text = format_value(value)?;
    Some(
        PluginResult::with_subtitle(text.clone(), "Calculator")
            .with_action(PluginAction::Copy { text }),
    )
}

/// Evaluate `query` to a usable value: valid expression *and* finite result.
///
/// `matches` and `result_for` share this predicate so activation and output
/// can never disagree — in particular `1 / 0` (valid but infinite) does not
/// activate the calculator.
fn finite_value(query: &str) -> Option<f64> {
    evaluate(query).filter(|value| value.is_finite())
}

/// Parse `query` as plain arithmetic and evaluate it.
///
/// The text is trimmed before parsing (the query shown in the search box is
/// never rewritten). `Expr::from_str` parses the grammar and the
/// [`Context::empty`] evaluation context exposes *no* constants and *no*
/// functions, so only number/operator expressions survive. Parse or
/// evaluation failures — incomplete input, stray words (`firefox`), unknown
/// variables — are ordinary outcomes and simply mean the calculator stays
/// inactive.
fn evaluate(query: &str) -> Option<f64> {
    let text = query.trim();
    if text.is_empty() {
        return None;
    }
    let expr: Expr = text.parse().ok()?;
    expr.eval_with_context(Context::empty()).ok()
}

/// Format an f64 into a clean, predictable display string.
///
/// Contract (documented behaviour):
/// - Non-finite values (`1 / 0` → `inf`, `0 / 0` → `nan`) produce no result.
/// - Values are rounded to at most [`MAX_DECIMALS`] (10) decimal digits,
///   which also hides floating-point noise such as `0.30000000000000004`.
/// - Integral values display without a decimal point (`256`, not `256.0`).
/// - `-0` displays as `0`.
fn format_value(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    let value = if value == 0.0 { 0.0 } else { value };
    let mut text = format!("{value:.prec$}", prec = MAX_DECIMALS);
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text.is_empty() {
        text = "0".to_owned();
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The title (the formatted value) of the calculator result, if any.
    fn result_text(query: &str) -> Option<String> {
        CalculatorPlugin.query(query).first().map(|r| r.title.clone())
    }

    #[test]
    fn basic_arithmetic() {
        assert_eq!(result_text("2 + 2").as_deref(), Some("4"));
        assert_eq!(result_text("10 - 3").as_deref(), Some("7"));
        assert_eq!(result_text("5 * 8").as_deref(), Some("40"));
        assert_eq!(result_text("100 / 4").as_deref(), Some("25"));
    }

    #[test]
    fn parentheses_and_power() {
        assert_eq!(result_text("(2 + 3) * 4").as_deref(), Some("20"));
        assert_eq!(result_text("2 ^ 8").as_deref(), Some("256"));
    }

    #[test]
    fn invalid_expressions_do_not_activate() {
        for query in ["2 +", "(5 * 3", "abc + 5", "2 ** 3", "echo: hi", ""] {
            assert!(!CalculatorPlugin.matches(query), "{query:?} must not match");
            assert!(CalculatorPlugin.query(query).is_empty(), "{query:?} must yield no results");
        }
    }

    #[test]
    fn normal_search_queries_do_not_activate() {
        for query in ["firefox", "visual studio code", "terminal", "file manager"] {
            assert!(!CalculatorPlugin.matches(query), "{query:?} must not match");
        }
    }

    #[test]
    fn constants_and_function_calls_are_not_exposed() {
        // The plugin intentionally supports only the six operators; pi, e and
        // the meval function library are left out.
        for query in ["pi", "e", "sqrt(16)", "sin(1)", "log(10)"] {
            assert!(!CalculatorPlugin.matches(query), "{query:?} must not match");
        }
    }

    #[test]
    fn whitespace_is_equivalent() {
        for query in ["2+2", "2 + 2", " 2 + 2 "] {
            assert_eq!(result_text(query).as_deref(), Some("4"), "query: {query:?}");
        }
    }

    #[test]
    fn decimal_values_format_cleanly() {
        assert_eq!(result_text("10 / 4").as_deref(), Some("2.5"));
        assert_eq!(
            result_text("0.1 + 0.2").as_deref(),
            Some("0.3"),
            "floating-point noise is rounded away"
        );
        assert_eq!(result_text("1 / 3").as_deref(), Some("0.3333333333"), "10 decimal digits max");
    }

    #[test]
    fn integral_results_have_no_decimal_point() {
        assert_eq!(result_text("2 ^ 8").as_deref(), Some("256"));
        assert_eq!(result_text("100 / 4").as_deref(), Some("25"));
        assert_eq!(result_text("2 + 2").as_deref(), Some("4"));
    }

    #[test]
    fn plain_numbers_are_valid_expressions() {
        // A bare number is a valid expression: typing `42` shows the value as
        // the first result and Enter copies it. This matches Raycast-style UX
        // — applications still follow below the calculator result.
        assert!(CalculatorPlugin.matches("42"));
        assert_eq!(result_text("42").as_deref(), Some("42"));
    }

    #[test]
    fn division_by_zero_yields_no_result() {
        // meval evaluates `1 / 0` to `inf`; non-finite values are not useful
        // answers, so the calculator stays quiet and apps keep searching.
        assert!(!CalculatorPlugin.matches("1 / 0"));
        assert!(CalculatorPlugin.query("1 / 0").is_empty());
    }

    #[test]
    fn negative_zero_displays_as_zero() {
        assert_eq!(result_text("-0").as_deref(), Some("0"));
    }

    #[test]
    fn the_result_carries_the_copy_action() {
        let result = CalculatorPlugin.query("2 + 2");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].title, "4");
        assert_eq!(result[0].subtitle.as_deref(), Some("Calculator"));
        assert_eq!(result[0].action, Some(PluginAction::Copy { text: "4".into() }));
    }

    #[test]
    fn matches_is_consistent_with_query_results() {
        for query in ["2^8", "(2 + 3) * 4", "2 +", "firefox", "10 / 4"] {
            assert_eq!(
                CalculatorPlugin.matches(query),
                !CalculatorPlugin.query(query).is_empty(),
                "matches and query must agree on {query:?}"
            );
        }
    }
}