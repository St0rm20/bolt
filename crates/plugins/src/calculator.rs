//! `calculator` — evaluates arithmetic expressions typed into the search box.
//!
//! The calculator activates *without a prefix*. Two situations light it up:
//!
//! * the query is a valid, finite expression (`2 + 2`, `2³`, `4//2`) — the
//!   plugin produces its single result — the formatted value — above any
//!   application matches, and Enter copies it to the clipboard via a
//!   [`PluginAction::Copy`]; or
//! * the query *looks like* an arithmetic expression still being typed
//!   (`6/`, `2 +`, `(5 * 3`) — the plugin then matches but yields no rows, so
//!   the launcher shows a single greyed-out "No results" hint instead of
//!   humming with an ordinary app search (`2 +` fuzzy-matches nothing useful).
//!
//! Ordinary searches (`firefox`) stay silent: letters are not arithmetic.
//!
//! ## The `//` root binder is a project convention
//!
//! `a//b` is **not** standard mathematical notation. This implementation
//! interprets it as *"the b-th root of a"*; `4//2` therefore means the square
//! root of 4, i.e. `2`. The parsing rule alone decides this (`//` is rewritten
//! to `a^(1/b)` in [`rewrite_root_operators`]), so changing the convention
//! later only touches that rewriter. The renaming of the binder is also
//! trivial if cross-platform users expect a different syntax.
//!
//! Expressions are parsed and evaluated with the `meval` crate (no shell, no
//! dynamic execution). A bare context — no constants, no function calls —
//! deliberately exposes only the arithmetic operators `+ - * / ^` (plus the
//! `//` root binder and superscript exponents, e.g. `16//4 == 2` and `2²`)
//! and parentheses; everything else (identifiers, stray symbols, incomplete
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

    /// No prefix: activation follows expression form. A valid finite
    /// expression activates the calculator; so does an *in-progress* expression
    /// that is not yet evaluable (`6/`), which lets the launcher show a hint
    /// rather than running an irrelevant app search. Ordinary words never
    /// activate it.
    fn matches(&self, query: &str) -> bool {
        finite_value(query).is_some() || looks_like_arithmetic(query)
    }

    fn query(&self, query: &str) -> Vec<PluginResult> {
        result_for(query).into_iter().collect()
    }
}

/// Whether `text` is plausibly an arithmetic expression still being typed:
/// only digits, the superscript digits/`⁻`, the operators `+ - * / ^`, the
/// `//` root binder and parentheses, with at least one digit present. A query
/// like `6/` or `2 +` passes; `firefox` or `echo: hi` do not.
///
/// This is deliberately a *character-class* check, not a partial parse — the
/// only goal is to keep malformed/incomplete expressions routed to the
/// calculator (which shows a hint) instead of the application index.
fn looks_like_arithmetic(text: &str) -> bool {
    let mut saw_digit = false;
    for ch in text.chars() {
        match ch {
            c if c.is_ascii_digit() => saw_digit = true,
            // Superscript digits are real material for a power expression.
            '⁰' | '¹' | '²' | '³' | '⁴' | '⁵' | '⁶' | '⁷' | '⁸' | '⁹' => saw_digit = true,
            c if c.is_whitespace() => {}
            '.' | '+' | '-' | '*' | '/' | '^' | '(' | ')' | '⁻' => {}
            _ => return false,
        }
    }
    saw_digit
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
/// never rewritten). Superscript exponents (`2²`) and the root binder
/// (`a//b` = the b-th root of `a`) are rewritten into ordinary power notation
/// first. `Expr::from_str` then parses the grammar and the
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
    let prepared = rewrite_root_operators(&normalize_superscripts(text));
    let expr: Expr = prepared.parse().ok()?;
    expr.eval_with_context(Context::empty()).ok()
}

/// Replace superscript digits with ordinary `^(...)` power notation, so `2²`
/// evaluates as `2^2` and `2⁻²` as `2^(-2)`. Non-superscript characters pass
/// through untouched.
fn normalize_superscripts(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    for ch in text.chars() {
        let mapped = match ch {
            '⁰' => Some('0'),
            '¹' => Some('1'),
            '²' => Some('2'),
            '³' => Some('3'),
            '⁴' => Some('4'),
            '⁵' => Some('5'),
            '⁶' => Some('6'),
            '⁷' => Some('7'),
            '⁸' => Some('8'),
            '⁹' => Some('9'),
            '⁻' => Some('-'),
            _ => None,
        };
        match mapped {
            Some(mapped) => run.push(mapped),
            None => {
                flush_superscript_run(&mut out, &mut run);
                out.push(ch);
            }
        }
    }
    flush_superscript_run(&mut out, &mut run);
    out
}

/// Emit a collected superscript run as `^(...)` and reset it.
fn flush_superscript_run(out: &mut String, run: &mut String) {
    if run.is_empty() {
        return;
    }
    let digits = std::mem::take(run);
    out.push('^');
    out.push('(');
    out.push_str(&digits);
    out.push(')');
}

/// A single arithmetic token for the root-operator rewriter.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    /// A number literal, kept verbatim for `meval` to parse.
    Num(String),
    /// An operator or parenthesis. `#` stands for the `//` root binder.
    Op(char),
}

/// The result of tokenizing a query for the rewriter.
enum Tokens {
    /// The query tokenised cleanly and ready for rewriting.
    Clean(Vec<Tok>),
    /// The query contains characters the rewriter cannot represent; the
    /// original text is used unchanged and the calculator stays inactive for
    /// it.
    Untouched,
}

/// Rewrite the binder operator `//` (b-th root of `a`: `a//b == a^(1/b)`)
/// into ordinary power notation that `meval` accepts. `//` binds like `^`
/// (tightest of all) and is right-associative, so `2^8//4` evaluates as
/// `(2^8)^(1/4) == 4` and `a//b//c` as `a//(b//c)`. Any character the
/// calculator does not support leaves the input untouched.
fn rewrite_root_operators(text: &str) -> String {
    let Tokens::Clean(mut tokens) = tokenize(text) else {
        return text.to_owned();
    };
    let mut iterations = 0;
    while iterations < 64 {
        iterations += 1;
        let Some(root) = tokens.iter().position(|token| token == &Tok::Op('#')) else {
            break;
        };
        let left = scan_root_left(&tokens, root);
        let right = scan_root_right(&tokens, root + 1);
        let mut rebuilt = Vec::with_capacity(tokens.len() + 8);
        rebuilt.extend_from_slice(&tokens[..left]);
        rebuilt.push(Tok::Op('('));
        rebuilt.extend_from_slice(&tokens[left..root]);
        rebuilt.push(Tok::Op(')'));
        rebuilt.push(Tok::Op('^'));
        rebuilt.push(Tok::Op('('));
        rebuilt.push(Tok::Num("1".to_owned()));
        rebuilt.push(Tok::Op('/'));
        rebuilt.push(Tok::Op('('));
        rebuilt.extend_from_slice(&tokens[root + 1..right]);
        rebuilt.push(Tok::Op(')'));
        rebuilt.push(Tok::Op(')'));
        rebuilt.extend_from_slice(&tokens[right..]);
        tokens = rebuilt;
    }
    join_tokens(&tokens)
}

/// The left operand of the root operator at `root` (index of the first
/// token of the operand, inclusive). `//` binds like `^`, so the operand
/// spans any surrounding power/root chain and unary signs, and stops at a
/// binary `+`/`-`/`*`/`/`.
fn scan_root_left(tokens: &[Tok], root: usize) -> usize {
    let mut depth = 0i32;
    let mut start = root;
    let mut i = root;
    loop {
        if i == 0 {
            break;
        }
        i -= 1;
        match &tokens[i] {
            Tok::Op(')') => {
                depth += 1;
                start = i;
            }
            Tok::Op('(') => {
                if depth > 0 {
                    depth -= 1;
                    start = i;
                } else {
                    start = i;
                    break;
                }
            }
            Tok::Op('^') | Tok::Op('#') => {
                start = i;
            }
            Tok::Num(_) => {
                if depth > 0 {
                    start = i;
                    continue;
                }
                start = i;
                if i == 0 {
                    break;
                }
                match &tokens[i - 1] {
                    // Power/root operators attach to this number...
                    Tok::Op('^') | Tok::Op('#') => {}
                    Tok::Op('+') | Tok::Op('-') => {
                        let unary = i == 1
                            || matches!(
                                tokens[i - 2],
                                Tok::Op('+') | Tok::Op('-') | Tok::Op('*') | Tok::Op('/') | Tok::Op('^') | Tok::Op('#') | Tok::Op('(')
                            );
                        if !unary {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            Tok::Op('+') | Tok::Op('-') => {
                if depth > 0 {
                    start = i;
                    continue;
                }
                if i == 0 {
                    start = i;
                    break;
                }
                if matches!(
                    tokens[i - 1],
                    Tok::Op('+') | Tok::Op('-') | Tok::Op('*') | Tok::Op('/') | Tok::Op('^') | Tok::Op('#') | Tok::Op('(')
                ) {
                    start = i;
                } else {
                    break;
                }
            }
            Tok::Op('*') | Tok::Op('/') => {
                if depth > 0 {
                    start = i;
                } else {
                    break;
                }
            }
            Tok::Op(_) => break,
        }
    }
    start
}

/// The right operand of the root operator at `root` (the first token of the
/// operand); returns the exclusive end index. Nested `//` and `^` chains are
/// part of the operand (right-associative), a binary `+`/`-`/`*`/`/` ends it.
fn scan_root_right(tokens: &[Tok], root: usize) -> usize {
    let mut depth = 0i32;
    let mut end = root;
    let mut started = false;
    let mut i = root;
    loop {
        if i >= tokens.len() {
            break;
        }
        match &tokens[i] {
            Tok::Op('(') => {
                depth += 1;
                end = i + 1;
                started = true;
                i += 1;
            }
            Tok::Op(')') => {
                if depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        end = i + 1;
                        break;
                    }
                    i += 1;
                } else {
                    break;
                }
            }
            Tok::Op('^') | Tok::Op('#') => {
                if depth > 0 {
                    i += 1;
                } else {
                    end = i + 1;
                    started = true;
                    i += 1;
                }
            }
            Tok::Num(_) => {
                end = i + 1;
                started = true;
                i += 1;
            }
            Tok::Op('+') | Tok::Op('-') => {
                if depth > 0 {
                    i += 1;
                    continue;
                }
                if !started {
                    // unary sign directly after the binder (`a//-2`)
                    end = i + 1;
                    started = true;
                    i += 1;
                } else {
                    break;
                }
            }
            Tok::Op('*') | Tok::Op('/') => {
                if depth > 0 {
                    i += 1;
                } else {
                    break;
                }
            }
            Tok::Op(_) => break,
        }
    }
    end
}

/// Tokenise a rewriter-friendly expression: numbers, the arithmetic
/// operators, parentheses and the `//` root binder. Any other character (a
/// letter, an exotic symbol) produces [`Tokens::Untouched`], meaning the
/// query is left alone and the calculator will not activate for it.
fn tokenize(text: &str) -> Tokens {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        match ch {
            c if c.is_ascii_whitespace() => i += 1,
            '(' | ')' | '+' | '-' | '*' | '^' => {
                tokens.push(Tok::Op(ch));
                i += 1;
            }
            '/' => {
                // A `//` pair is the root binder; a lone `/` stays division.
                if chars.get(i + 1) == Some(&'/') {
                    tokens.push(Tok::Op('#'));
                    i += 2;
                } else {
                    tokens.push(Tok::Op('/'));
                    i += 1;
                }
            }
            c if c.is_ascii_digit() || c == '.' => {
                let mut number = String::new();
                while i < chars.len() {
                    let c = chars[i];
                    if c.is_ascii_digit() || c == '.' {
                        number.push(c);
                        i += 1;
                    } else if (c == 'e' || c == 'E')
                        && (chars.get(i + 2).is_some_and(|d| d.is_ascii_digit()))
                        && (chars.get(i + 1).is_some_and(|n| n.is_ascii_digit())
                            || chars.get(i + 1) == Some(&'-')
                            || chars.get(i + 1) == Some(&'+'))
                    {
                        number.push(c);
                        i += 1;
                        if let Some(sign) = chars.get(i) {
                            if *sign == '-' || *sign == '+' {
                                number.push(*sign);
                                i += 1;
                            }
                        }
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            number.push(chars[i]);
                            i += 1;
                        }
                    } else {
                        break;
                    }
                }
                tokens.push(Tok::Num(number));
            }
            _ => return Tokens::Untouched,
        }
    }
    Tokens::Clean(tokens)
}

/// Concatenate tokens back into an expression string for `meval`.
fn join_tokens(tokens: &[Tok]) -> String {
    let mut out = String::new();
    for token in tokens {
        match token {
            Tok::Num(number) => out.push_str(number),
            Tok::Op(op) => out.push(*op),
        }
    }
    out
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
    fn complete_expressions_activate_with_a_result() {
        for query in ["2 + 2", "(2 + 3) * 4", "6/2", "2^8", "10 / 4"] {
            assert!(CalculatorPlugin.matches(query), "{query:?} must match");
            assert_eq!(CalculatorPlugin.query(query).len(), 1, "{query:?} yields one row");
        }
    }

    #[test]
    fn incomplete_expressions_activate_without_rows() {
        // An in-progress expression (`6/`, `2 +`, an unclosed parenthesis) is
        // routed to the calculator so the launcher can show a single hint —
        // the plugin *matches* yet produces no result row.
        for query in ["6/", "2 +", "(5 * 3", "2 ** 3", "16//"] {
            assert!(CalculatorPlugin.matches(query), "{query:?} must match as arithmetic in progress");
            assert!(CalculatorPlugin.query(query).is_empty(), "{query:?} must yield no rows yet");
        }
    }

    #[test]
    fn non_arithmetic_queries_never_activate() {
        for query in ["echo: hi", "abc + 5", "firefox", "visual studio code", "pi", "sqrt(16)", ""] {
            assert!(!CalculatorPlugin.matches(query), "{query:?} must not match");
            assert!(CalculatorPlugin.query(query).is_empty(), "{query:?} must yield no results");
        }
    }

    #[test]
    fn normal_search_queries_do_not_activate() {
        for query in ["firefox", "visual studio code", "terminal", "file manager"] {
            assert!(!CalculatorPlugin.matches(query), "{query:?} must not match");
            assert!(CalculatorPlugin.query(query).is_empty(), "{query:?} must yield no rows");
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
    fn division_by_zero_yields_a_hint_but_no_result() {
        // meval evaluates `1 / 0` to `inf`; non-finite values are not useful
        // answers, so the calculator produces no row. The query is still
        // arithmetic-looking, so it activates for a single "No results" hint
        // rather than sending Apps on a futile fuzzy search.
        assert!(CalculatorPlugin.matches("1 / 0"));
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
    fn matches_covers_both_results_and_in_progress_hints() {
        // A valid expression matches and yields a row; an in-progress
        // expression matches but yields none (the launcher turns that into a
        // hint); a non-arithmetic query stays silent entirely.
        assert!(CalculatorPlugin.matches("6/2"));
        assert_eq!(CalculatorPlugin.query("6/2").len(), 1);
        assert!(CalculatorPlugin.matches("6/"));
        assert!(CalculatorPlugin.query("6/").is_empty());
        assert!(!CalculatorPlugin.matches("firefox"));
    }

    #[test]
    fn superscript_exponents_are_supported() {
        assert_eq!(result_text("2²").as_deref(), Some("4"));
        assert_eq!(result_text("2³").as_deref(), Some("8"));
        assert_eq!(result_text("2¹⁰").as_deref(), Some("1024"));
        assert_eq!(result_text("2⁻²").as_deref(), Some("0.25"));
    }

    #[test]
    fn root_operator_evaluates_bth_roots() {
        assert_eq!(result_text("16//4").as_deref(), Some("2"));
        assert_eq!(result_text("27//3").as_deref(), Some("3"));
        assert_eq!(result_text("81//4").as_deref(), Some("3"));
        assert_eq!(result_text("2^8//4").as_deref(), Some("4"));
    }

    #[test]
    fn root_operator_binds_like_power_and_parentheses_work() {
        // `//` binds tighter than the binary operators: 2//3, then +1.
        assert_eq!(result_text("2//3+1").as_deref(), Some("2.2599210499"));
        // `^` binds through the root: 8//2^2 == 8^(1/4).
        assert_eq!(result_text("8//2^2").as_deref(), Some("1.6817928305"));
        // A parenthesised left operand is a clean single operand.
        assert_eq!(result_text("(1+3)//2").as_deref(), Some("2"));
    }

    #[test]
    fn root_and_superscript_expressions_activate_like_plain_arithmetic() {
        for query in ["16//4", "2²", "2^8//4", "8//2^2", "2//3+1"] {
            assert!(CalculatorPlugin.matches(query), "{query:?} must activate");
            assert_eq!(CalculatorPlugin.query(query).len(), 1, "{query:?} yields one row");
        }
    }

    #[test]
    fn degenerate_root_inputs_stay_inactive_or_hint_only() {
        // A root with a letter operand is not arithmetic; `//` alone has no
        // digits, so neither activates.
        for query in ["16//square", "//"] {
            assert!(!CalculatorPlugin.matches(query), "{query:?} must not activate");
        }
        // `2^^//` and `8//` are arithmetic-looking but unparseable: they
        // match (as a hint) yet yield no rows.
        for query in ["2^^//", "8//"] {
            assert!(CalculatorPlugin.matches(query), "{query:?} must match as in-progress input");
            assert!(CalculatorPlugin.query(query).is_empty(), "{query:?} must yield no rows");
        }
    }
}