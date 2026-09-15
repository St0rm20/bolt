//! Application configuration, serialized to/from `config/config.toml`.
//!
//! Configuration parsing deliberately lives in the `core` crate so it stays
//! independent of the GTK UI.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

/// Top-level launcher configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// Keyboard shortcut used to open the launcher.
    pub shortcut: String,
    /// UI theme applied to the launcher window.
    ///
    /// Any value other than `light` maps to the dark palette; the special
    /// value `auto` follows the system (XDG portal) color-scheme preference.
    /// Only used when [`Appearance::theme`] is empty.
    pub theme: String,
    /// Names of the plugins enabled in this installation.
    #[serde(default)]
    pub enabled_plugins: Vec<String>,
    /// Visual appearance tuning. Absent from older config files; falls back
    /// to the values of [`Appearance::default`].
    #[serde(default)]
    pub appearance: Appearance,
}

/// Visual appearance of the launcher window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Appearance {
    /// Theme of the launcher: `auto` (follow the system), `light` or `dark`.
    /// The empty string means "use the top-level [`Config::theme`]".
    pub theme: String,
    /// Accent colour as a CSS hex string, e.g. `#0a84ff`.
    pub accent_color: String,
    /// Whether the window background is translucent (blur is applied by the
    /// compositor, not the app). When disabled the background is opaque.
    pub blur_enabled: bool,
    /// Corner radius of the launcher window in pixels.
    pub corner_radius: u32,
    /// Default width of the launcher window in pixels.
    pub window_width: u32,
    /// Whether the clipboard history persists between sessions. When enabled
    /// (the default), copied text is saved to
    /// `$XDG_DATA_HOME/launcher/clipboard_history.json` so `clip:` shows past
    /// copies after a restart.
    pub clipboard_persistence: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: String::new(),
            accent_color: "#0a84ff".to_owned(),
            blur_enabled: true,
            corner_radius: 16,
            window_width: 640,
            clipboard_persistence: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shortcut: "SUPER+SPACE".to_owned(),
            theme: "dark".to_owned(),
            enabled_plugins: Vec::new(),
            appearance: Appearance::default(),
        }
    }
}

impl Config {
    /// Load configuration from a TOML file on disk.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path.as_ref())?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// The theme choice that should actually be applied.
    ///
    /// `[appearance].theme` wins when set; otherwise the historical top-level
    /// `theme` key is used, so older config files keep working unchanged.
    pub fn effective_theme(&self) -> &str {
        if self.appearance.theme.is_empty() {
            &self.theme
        } else {
            &self.appearance.theme
        }
    }
}

/// Errors that can occur while loading the configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// Failed to read the configuration file from disk.
    Io(std::io::Error),
    /// The file content is not valid TOML or does not match [`Config`].
    Parse(toml::de::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(err) => write!(f, "failed to read config file: {err}"),
            ConfigError::Parse(err) => write!(f, "failed to parse config file: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Io(err) => Some(err),
            ConfigError::Parse(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for ConfigError {
    fn from(err: std::io::Error) -> Self {
        ConfigError::Io(err)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(err: toml::de::Error) -> Self {
        ConfigError::Parse(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_toml() {
        let input = r#"
shortcut = "SUPER+SPACE"
theme = "dark"
enabled_plugins = ["applications", "calculator"]
"#;
        let config: Config = toml::from_str(input).expect("valid TOML should parse");
        assert_eq!(config.shortcut, "SUPER+SPACE");
        assert_eq!(config.theme, "dark");
        assert_eq!(config.enabled_plugins, ["applications", "calculator"]);
    }

    #[test]
    fn missing_plugins_defaults_to_empty() {
        let input = "shortcut = \"SUPER+SPACE\"\ntheme = \"light\"\n";
        let config: Config = toml::from_str(input).expect("valid TOML should parse");
        assert!(config.enabled_plugins.is_empty());
        assert_eq!(config.appearance, Appearance::default());
    }

    #[test]
    fn parses_appearance_section() {
        let input = r##"
shortcut = "SUPER+SPACE"
theme = "dark"

[appearance]
theme = "auto"
accent_color = "#ff6600"
blur_enabled = false
corner_radius = 24
window_width = 800
clipboard_persistence = false
"##;
        let config: Config = toml::from_str(input).expect("valid TOML should parse");
        assert_eq!(config.appearance.theme, "auto");
        assert_eq!(config.appearance.accent_color, "#ff6600");
        assert!(!config.appearance.blur_enabled);
        assert_eq!(config.appearance.corner_radius, 24);
        assert_eq!(config.appearance.window_width, 800);
        assert!(!config.appearance.clipboard_persistence);
    }

    #[test]
    fn missing_appearance_fields_fall_back_to_defaults() {
        let input = r##"
shortcut = "SUPER+SPACE"
theme = "dark"

[appearance]
accent_color = "#123456"
"##;
        let config: Config = toml::from_str(input).expect("valid TOML should parse");
        assert_eq!(config.appearance.accent_color, "#123456");
        assert_eq!(config.appearance.blur_enabled, true);
        assert_eq!(config.appearance.corner_radius, 16);
        assert_eq!(config.appearance.window_width, 640);
        assert!(config.appearance.clipboard_persistence, "persistence on by default");
        assert_eq!(config.appearance.theme, "");
    }

    #[test]
    fn effective_theme_prefers_the_appearance_section() {
        let mut config = Config::default();
        assert_eq!(config.effective_theme(), "dark");

        config.theme = "light".to_owned();
        config.appearance.theme = "auto".to_owned();
        assert_eq!(config.effective_theme(), "auto");

        config.appearance.theme.clear();
        assert_eq!(config.effective_theme(), "light");
    }
}