//! Visual appearance of the launcher: theme resolution, colour palettes and
//! the CSS that gives the window its translucent, rounded, Raycast-like look.
//!
//! Theme resolution
//! -----------------
//! [`resolve_theme`] turns the config's theme choice (`light`, `dark` or
//! `auto`) into one concrete palette. `auto` follows the *system* colour
//! scheme, which is read from the XDG desktop portal
//! (`org.freedesktop.portal.Settings`, key `org.freedesktop.appearance`
//! / `color-scheme`) through GIO's synchronous D-Bus proxy. When the portal
//! is unavailable (typical on minimal Hyprland setups) `auto` falls back to
//! `dark`, which is the project's default look.
//!
//! Live tracking of the system scheme's *changes* is intentionally left for a
//! later iteration: subscribing to the portal's `SettingChanged` signal would
//! pull in a heavier D-Bus runtime, and GTK 4.18 (this stack) does not support
//! the `prefers-color-scheme` CSS media query (verified empirically), so a
//! one-shot query at startup is the deterministic option today.

use gio::prelude::*;
use gtk4::glib;
use gtk4::glib::variant::ToVariant;
use gtk4::prelude::*;

/// Which system colour scheme was reported by the portal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemColorScheme {
    Light,
    Dark,
}

/// A resolved colour palette for one theme.
#[derive(Debug)]
struct Palette {
    /// Translucent background used when compositor blur is enabled.
    background_blur: &'static str,
    /// Opaque background used when blur is disabled.
    background_solid: &'static str,
    /// Primary text colour.
    foreground: &'static str,
    /// Secondary text colour (placeholders, empty-state messages).
    secondary: &'static str,
    /// 1px edge highlight/glow faking the separation macOS panels have.
    edge: &'static str,
    /// Search-entry background: a subtle elevation over the surface, not a
    /// saturated accent rectangle.
    elevated: &'static str,
    /// 1px border of the search entry.
    elevated_border: &'static str,
    /// Row hover highlight (translucent, palette-consistent).
    hover: &'static str,
}

const DARK: Palette = Palette {
    background_blur: "rgba(28, 28, 30, 0.72)",
    background_solid: "#1c1c1e",
    foreground: "#f5f5f7",
    secondary: "#a1a1a6",
    edge: "rgba(255, 255, 255, 0.12)",
    elevated: "rgba(255, 255, 255, 0.08)",
    elevated_border: "rgba(255, 255, 255, 0.16)",
    hover: "rgba(255, 255, 255, 0.07)",
};

const LIGHT: Palette = Palette {
    background_blur: "rgba(255, 255, 255, 0.75)",
    background_solid: "#ffffff",
    foreground: "#1c1c1e",
    secondary: "#6e6e73",
    edge: "rgba(0, 0, 0, 0.12)",
    elevated: "rgba(0, 0, 0, 0.06)",
    elevated_border: "rgba(0, 0, 0, 0.16)",
    hover: "rgba(0, 0, 0, 0.05)",
};

/// Default accent colour (Apple's standard blue).
const DEFAULT_ACCENT: &str = "#0a84ff";

/// Resolve the effective theme: `light`, `dark`, or `auto` resolved against
/// the system colour scheme (falling back to `dark` when unknown).
#[must_use]
pub fn resolve_theme(theme_choice: &str, system: Option<SystemColorScheme>) -> &'static str {
    match theme_choice.trim().to_ascii_lowercase().as_str() {
        "light" => "light",
        "dark" => "dark",
        "auto" => match system {
            Some(SystemColorScheme::Dark) => "dark",
            Some(SystemColorScheme::Light) => "light",
            // The portal is typically unavailable on minimal Hyprland setups;
            // fall back to the project's default look.
            None => "dark",
        },
        _ => "dark",
    }
}

/// Read the system colour scheme from the XDG desktop portal. Returns `None`
/// when the portal is unreachable or has no preference, in which case the
/// caller directs the fallback.
#[must_use]
pub fn detect_system_color_scheme() -> Option<SystemColorScheme> {
    let flags = gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES | gio::DBusProxyFlags::DO_NOT_AUTO_START;
    let proxy = gio::DBusProxy::for_bus_sync(
        gio::BusType::Session,
        flags,
        None::<&gio::DBusInterfaceInfo>,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
        None::<&gio::Cancellable>,
    )
    .ok()?;

    // Read(s group, s key) -> v
    let arguments = glib::Variant::tuple_from_iter([
        "org.freedesktop.appearance".to_variant(),
        "color-scheme".to_variant(),
    ]);
    let reply = proxy
        .call_sync("Read", Some(&arguments), gio::DBusCallFlags::NONE, -1, None::<&gio::Cancellable>)
        .ok()?;
    match unwrap_single_out_argument(reply).get::<u32>()? {
        1 => Some(SystemColorScheme::Dark),
        2 => Some(SystemColorScheme::Light),
        _ => None,
    }
}

/// Apply the resolved theme to the window: install the launcher CSS provider
/// and nudge GTK itself towards the matching dark/light preference so the
/// standard widgets (scrollbars, entries, ...) follow along.
pub fn apply<W: IsA<gtk4::Widget>>(
    window: &W,
    appearance: &launcher_core::config::Appearance,
    theme: &str,
) {
    let palette = if theme == "light" { &LIGHT } else { &DARK };
    let css = build_css(palette, appearance);
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(&css);
    let display = window.display();
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    if let Some(settings) = gtk4::Settings::default() {
        settings.set_gtk_application_prefer_dark_theme(theme == "dark");
    }
}

/// Build the launcher stylesheet from a resolved palette and the appearance
/// configuration. Kept separate from [`apply`] so it is unit-testable.
fn build_css(palette: &Palette, appearance: &launcher_core::config::Appearance) -> String {
    let background = if appearance.blur_enabled {
        palette.background_blur
    } else {
        palette.background_solid
    };
    let accent = valid_hex(&appearance.accent_color).unwrap_or(DEFAULT_ACCENT);
    let selection = tint(accent, 0.35);
    let focus_border = tint(accent, 0.65);
    let focus_ring = tint(accent, 0.16);

    format!(
        r#"
/* Toplevel card. Keeps a translucent background when blur is enabled so the
   compositor (Hyprland) blurs whatever sits behind the window; the surface
   below paints the same colour, and the nested list stays transparent so no
   opaque sheet is ever drawn on top of it. */
window.launcher {{
    background-color: {background};
    color: {fg};
    border-radius: {radius}px;
    border: 1px solid {edge};
    box-shadow: inset 0 0 24px rgba(0, 0, 0, 0.08);
}}

/* Content surface: owns the same translucent background (belt-and-braces with
   the window) and scales in from 96% to 100%. Removing `.launcher-scale-in`
   after the window is mapped triggers this transition. */
.launcher-surface {{
    background-color: {background};
    color: {fg};
    border-radius: {radius}px;
    padding: 8px 8px 12px 8px;
    transition: transform 140ms cubic-bezier(0.33, 1, 0.68, 1);
}}

.launcher-surface.launcher-scale-in {{
    transform: scale(0.96);
}}

/* Undo the default theme's opaque backgrounds on the widgets nested inside
   the surface: GtkListBox (`list`), GtkScrolledWindow and GtkViewport paint
   an opaque white/gray sheet by default which would cover the translucent
   card and make the light text unreadable. A transparent parent alone does
   not stop these. */
window.launcher list,
window.launcher scrolledwindow,
window.launcher viewport {{
    background-color: transparent;
}}

/* Search entry: an elevated field that belongs to the launcher card — no
   saturated accent rectangle. Its focus state is a subtle ring, not a
   highlight that dominates the window. */
.launcher-search {{
    font-size: 16px;
    padding: 9px 14px;
    border-radius: 10px;
    background-color: {elevated};
    border: 1px solid {elevated_border};
    color: {fg};
    caret-color: {accent};
    outline: none;
    box-shadow: none;
}}

.launcher-search:hover {{
    background-color: {hover};
}}

.launcher-search:focus,
.launcher-search:focus-within {{
    background-color: {elevated};
    border-color: {focus_border};
    box-shadow: 0 0 0 2px {focus_ring};
}}

.launcher-search placeholder,
.launcher-search image {{
    color: {secondary};
}}

.launcher-results {{
    margin-top: 6px;
}}

.launcher-results-row {{
    padding: 7px 14px;
    border-radius: 10px;
    background-color: transparent;
    color: {fg};
}}

.launcher-results-row:hover {{
    background-color: {hover};
}}

/* Selected row: a translucent accent tint — obvious keyboard focus without
   an opaque saturated slab dominating the launcher. */
.launcher-results-row:selected,
.launcher-results-row.selected,
.launcher-results-row:selected:hover,
.launcher-results-row.selected:hover {{
    background-color: {selection};
    color: {fg};
}}

.launcher-results-row:selected .launcher-app-name,
.launcher-results-row.selected .launcher-app-name {{
    color: {fg};
}}

.launcher-app-name {{
    font-size: 15px;
    color: {fg};
}}

.launcher-result-subtitle {{
    font-size: 12px;
    color: {secondary};
}}

.launcher-result-icon {{
    opacity: 1;
}}

.launcher-message {{
    padding: 14px 16px;
    color: {secondary};
    font-size: 15px;
}}

/* Hint rows (a plugin matched but produced no results): secondary colour so
   they read as guidance, never as actionable rows. */
.launcher-hint {{
    padding: 10px 16px;
}}

.launcher-hint-text {{
    color: {secondary};
    font-size: 13px;
    font-style: italic;
}}
"#,
        background = background,
        fg = palette.foreground,
        secondary = palette.secondary,
        accent = accent,
        selection = selection,
        focus_border = focus_border,
        focus_ring = focus_ring,
        elevated = palette.elevated,
        elevated_border = palette.elevated_border,
        hover = palette.hover,
        edge = palette.edge,
        radius = appearance.corner_radius,
    )
}

/// GDBus can hand back a single `v`-typed out argument either directly or
/// wrapped in a `(v)` tuple. Unwrap whichever layer is present so the caller
/// lands on the concrete value (here: the `color-scheme` u32).
fn unwrap_single_out_argument(value: glib::Variant) -> glib::Variant {
    let mut value = if value.type_().as_str() == "(v)" {
        value.child_value(0)
    } else {
        value
    };
    if value.type_().as_str() == "v" {
        value = value.child_value(0);
    }
    value
}

/// Normalise a `#rrggbb` colour. Returns `None` for anything else.
fn valid_hex(input: &str) -> Option<&str> {
    let input = input.trim();
    let digits = input.strip_prefix('#')?;
    if digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(input)
    } else {
        None
    }
}

/// Produce `rgba(r, g, b, alpha)` from a `#rrggbb` colour.
fn tint(hex: &str, alpha: f32) -> String {
    let digits = &hex[1..];
    let red = u8::from_str_radix(&digits[0..2], 16).unwrap_or(0x0a);
    let green = u8::from_str_radix(&digits[2..4], 16).unwrap_or(0x84);
    let blue = u8::from_str_radix(&digits[4..6], 16).unwrap_or(0xff);
    format!("rgba({red}, {green}, {blue}, {alpha})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_themes_are_resolved_directly() {
        assert_eq!(resolve_theme("light", Some(SystemColorScheme::Dark)), "light");
        assert_eq!(resolve_theme("dark", Some(SystemColorScheme::Light)), "dark");
        assert_eq!(resolve_theme("  LIGHT  ", None), "light");
    }

    #[test]
    fn auto_follows_the_system() {
        assert_eq!(resolve_theme("auto", Some(SystemColorScheme::Dark)), "dark");
        assert_eq!(resolve_theme("auto", Some(SystemColorScheme::Light)), "light");
    }

    #[test]
    fn auto_without_portal_falls_back_to_dark() {
        assert_eq!(resolve_theme("auto", None), "dark");
        assert_eq!(resolve_theme("bogus", None), "dark");
    }

    #[test]
    fn invalid_accent_falls_back_to_the_default() {
        assert_eq!(valid_hex("#0a84ff"), Some("#0a84ff"));
        assert_eq!(valid_hex("0a84ff"), None);
        assert_eq!(valid_hex("#xyzabc"), None);
        assert_eq!(valid_hex(""), None);
    }

    #[test]
    fn tint_produces_an_rgba_string() {
        assert_eq!(tint("#0a84ff", 0.28), "rgba(10, 132, 255, 0.28)");
        assert_eq!(tint("#ffffff", 1.0), "rgba(255, 255, 255, 1)");
    }

    #[test]
    fn css_includes_background_and_radius() {
        let appearance = launcher_core::config::Appearance::default();
        let css = build_css(&DARK, &appearance);
        assert!(css.contains("background-color: rgba(28, 28, 30, 0.72)"));
        assert!(css.contains("border-radius: 16px"));
        assert!(css.contains("#0a84ff"));
    }

    #[test]
    fn css_includes_edge_glow_and_scale_transition() {
        let appearance = launcher_core::config::Appearance::default();
        let css = build_css(&DARK, &appearance);
        assert!(css.contains("border: 1px solid rgba(255, 255, 255, 0.12)"));
        assert!(css.contains("transform: scale(0.96)"));
        assert!(css.contains("transition: transform 140ms"));
    }

    #[test]
    fn css_includes_the_plugin_subtitle_style() {
        let appearance = launcher_core::config::Appearance::default();
        let css = build_css(&DARK, &appearance);
        assert!(css.contains(".launcher-result-subtitle"));
        assert!(css.contains("font-size: 12px"));
        assert!(css.contains("color: #a1a1a6"));
    }

    #[test]
    fn css_includes_the_hint_row_styles() {
        let appearance = launcher_core::config::Appearance::default();
        let css = build_css(&DARK, &appearance);
        assert!(css.contains(".launcher-hint"));
        assert!(css.contains(".launcher-hint-text"));
        assert!(css.contains("font-style: italic"));
    }

    #[test]
    fn nested_list_widgets_are_forced_transparent() {
        let appearance = launcher_core::config::Appearance::default();
        let css = build_css(&DARK, &appearance);
        assert!(css.contains("window.launcher list"));
        assert!(css.contains("window.launcher scrolledwindow"));
        assert!(css.contains("window.launcher viewport"));
        assert!(css.contains("background-color: transparent"));
    }

    #[test]
    fn selected_row_and_focus_use_translucent_accent_tints() {
        let appearance = launcher_core::config::Appearance::default();
        let css = build_css(&DARK, &appearance);
        assert!(css.contains("rgba(10, 132, 255, 0.35)"));
        assert!(css.contains(".launcher-results-row:selected"));
        assert!(css.contains(".launcher-search:focus-within"));
    }

    #[test]
    fn icons_are_not_transparent() {
        let appearance = launcher_core::config::Appearance::default();
        let css = build_css(&DARK, &appearance);
        assert!(css.contains("opacity: 1"));
        assert!(!css.contains("opacity: 0.9"));
    }

    #[test]
    fn light_theme_uses_dark_edge_glow() {
        let appearance = launcher_core::config::Appearance::default();
        let css = build_css(&LIGHT, &appearance);
        assert!(css.contains("border: 1px solid rgba(0, 0, 0, 0.12)"));
    }

    #[test]
    fn blur_disabled_uses_an_opaque_background() {
        let mut appearance = launcher_core::config::Appearance::default();
        appearance.blur_enabled = false;
        let css = build_css(&LIGHT, &appearance);
        assert!(css.contains("background-color: #ffffff"));
        assert!(!css.contains("rgba(255"));
    }
}