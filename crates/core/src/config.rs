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
    /// Clipboard-history retention. Absent from older config files; falls
    /// back to [`ClipboardConfig::default`].
    #[serde(default)]
    pub clipboard: ClipboardConfig,
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

/// How long the launcher keeps clipboard history.
///
/// `Session` keeps a memory-only history: nothing is written to disk and the
/// entries live for the current daemon run only. The other values bound the
/// *age* of kept entries (older ones are pruned) and enable the on-disk
/// history file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardRetention {
    /// In-memory only: no persistence, entries live for the session only.
    #[serde(rename = "session")]
    Session,
    /// Keep entries up to one day.
    #[serde(rename = "1_day")]
    OneDay,
    /// Keep entries up to one week.
    #[serde(rename = "1_week")]
    OneWeek,
    /// Keep entries up to one month.
    #[serde(rename = "1_month")]
    OneMonth,
}

impl ClipboardRetention {
    /// Every retention value, in the order the settings panel shows them.
    pub const ALL: [ClipboardRetention; 4] = [
        ClipboardRetention::Session,
        ClipboardRetention::OneDay,
        ClipboardRetention::OneWeek,
        ClipboardRetention::OneMonth,
    ];

    /// The retention duration in seconds. `None` for
    /// [`ClipboardRetention::Session`] — the history is memory-only and never
    /// persisted.
    #[must_use]
    pub fn as_seconds(self) -> Option<u64> {
        match self {
            ClipboardRetention::Session => None,
            ClipboardRetention::OneDay => Some(86_400),
            ClipboardRetention::OneWeek => Some(604_800),
            ClipboardRetention::OneMonth => Some(2_592_000),
        }
    }

    /// The config-file spelling, e.g. `"1_week"`.
    #[must_use]
    pub fn as_toml(self) -> &'static str {
        match self {
            ClipboardRetention::Session => "session",
            ClipboardRetention::OneDay => "1_day",
            ClipboardRetention::OneWeek => "1_week",
            ClipboardRetention::OneMonth => "1_month",
        }
    }

    /// A short human-readable label for the settings panel.
    #[must_use]
    pub fn as_label(self) -> &'static str {
        match self {
            ClipboardRetention::Session => "Session only",
            ClipboardRetention::OneDay => "1 day",
            ClipboardRetention::OneWeek => "1 week",
            ClipboardRetention::OneMonth => "1 month",
        }
    }

    /// Parse a config-file value. Accepts the canonical spellings
    /// (`session`, `1_day`, `1_week`, `1_month`), case-insensitively.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "session" => Some(ClipboardRetention::Session),
            "1_day" => Some(ClipboardRetention::OneDay),
            "1_week" => Some(ClipboardRetention::OneWeek),
            "1_month" => Some(ClipboardRetention::OneMonth),
            _ => None,
        }
    }
}

/// The `[clipboard]` section of the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipboardConfig {
    /// How long copied text is kept. Defaults to a week, which preserves the
    /// always-on persistence behaviour of earlier versions.
    pub retention: ClipboardRetention,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            retention: ClipboardRetention::OneWeek,
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
            clipboard: ClipboardConfig::default(),
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

    /// Update a single `section.key` string value in a TOML config file on
    /// disk, preserving every other key. A missing file or section is
    /// created. The value is stored as a TOML string (properly quoted and
    /// escaped when serialised).
    ///
    /// Used by the launcher's settings panel to persist theme and retention
    /// choices made at runtime.
    pub fn set_toml_string(
        path: impl AsRef<Path>,
        section: &str,
        key: &str,
        value: &str,
    ) -> Result<(), ConfigError> {
        let path = path.as_ref();
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(ConfigError::Io(err)),
        };
        let mut doc: toml::Value = if contents.trim().is_empty() {
            toml::Value::Table(toml::map::Map::new())
        } else {
            toml::from_str(&contents)?
        };
        let root = doc.as_table_mut().ok_or_else(non_table_error)?;
        let section_value = root
            .entry(section.to_owned())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let table = section_value.as_table_mut().ok_or_else(non_table_error)?;
        table.insert(key.to_owned(), toml::Value::String(value.to_owned()));
        let written = toml::to_string(&doc).unwrap_or(contents);
        std::fs::write(path, written).map_err(ConfigError::Io)
    }

    /// Escape `value` as a TOML string literal (`"..."`, escaping embedded
    /// quotes/backslashes) — the exact form JSON-free writing of a config
    /// value needs.
    #[must_use]
    pub fn toml_string_literal(value: &str) -> String {
        toml::to_string(&toml::Value::String(value.to_owned()))
            .unwrap_or_else(|_| format!("{value:?}"))
            .trim_end()
            .to_owned()
    }
}

/// The config file is structurally broken for [`Config::set_toml_string`]
/// (a non-table root or section), reported as invalid data on the path being
/// written.
fn non_table_error() -> ConfigError {
    ConfigError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "config root and sections must be tables",
    ))
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

    #[test]
    fn retention_values_round_trip_through_the_config_spellings() {
        for retention in ClipboardRetention::ALL {
            assert_eq!(ClipboardRetention::parse(retention.as_toml()), Some(retention));
            assert_eq!(ClipboardRetention::parse(&retention.as_toml().to_uppercase()), Some(retention));
        }
        assert_eq!(ClipboardRetention::parse("bogus"), None);
        assert_eq!(ClipboardRetention::parse(""), None);
    }

    #[test]
    fn session_retention_disables_persistence_others_do_not() {
        assert_eq!(ClipboardRetention::Session.as_seconds(), None);
        assert_eq!(ClipboardRetention::OneDay.as_seconds(), Some(86_400));
        assert_eq!(ClipboardRetention::OneWeek.as_seconds(), Some(604_800));
        assert_eq!(ClipboardRetention::OneMonth.as_seconds(), Some(2_592_000));
        assert_eq!(ClipboardRetention::OneWeek.as_label(), "1 week");
        assert_eq!(ClipboardRetention::Session.as_label(), "Session only");
    }

    #[test]
    fn parses_clipboard_section() {
        let input = "shortcut = \"SUPER+SPACE\"\ntheme = \"dark\"\n[clipboard]\nretention = \"1_month\"\n";
        let config: Config = toml::from_str(input).expect("valid TOML should parse");
        assert_eq!(config.clipboard.retention, ClipboardRetention::OneMonth);

        let input = "shortcut = \"SUPER+SPACE\"\ntheme = \"dark\"\n[clipboard]\nretention = \"session\"\n";
        let config: Config = toml::from_str(input).expect("valid TOML should parse");
        assert_eq!(config.clipboard.retention, ClipboardRetention::Session);
    }

    #[test]
    fn missing_clipboard_section_defaults_to_a_week() {
        let input = "shortcut = \"SUPER+SPACE\"\ntheme = \"dark\"\n";
        let config: Config = toml::from_str(input).expect("valid TOML should parse");
        assert_eq!(config.clipboard, ClipboardConfig::default());
        assert_eq!(config.clipboard.retention, ClipboardRetention::OneWeek);
    }

    #[test]
    fn set_toml_string_updates_an_existing_key() {
        let dir = temp_dir("set-string");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "shortcut = \"SUPER+SPACE\"\ntheme = \"dark\"\n[appearance]\ntheme = \"auto\"\n",
        )
        .expect("write");
        Config::set_toml_string(&path, "appearance", "theme", "light").expect("write ok");
        let config = Config::load(&path).expect("load ok");
        assert_eq!(config.appearance.theme, "light");
        assert_eq!(config.shortcut, "SUPER+SPACE", "unrelated keys survive");
    }

    #[test]
    fn set_toml_string_creates_missing_sections_and_files() {
        let dir = temp_dir("set-new");
        let path = dir.join("config.toml");
        std::fs::write(&path, "shortcut = \"SUPER+SPACE\"\ntheme = \"dark\"\n").expect("seed file");
        Config::set_toml_string(&path, "clipboard", "retention", "1_day").expect("write ok");
        let config = Config::load(&path).expect("load ok");
        assert_eq!(config.clipboard.retention, ClipboardRetention::OneDay);
        assert_eq!(config.shortcut, "SUPER+SPACE", "unrelated keys survive");

        // A brand-new file is created with only the requested key; it is not
        // a complete config yet, so the round-trip asserts the on-disk text
        // rather than a `Config::load` (which needs the top-level defaults).
        let fresh = dir.join("fresh.toml");
        Config::set_toml_string(&fresh, "clipboard", "retention", "session").expect("file created");
        let contents = std::fs::read_to_string(&fresh).expect("file exists");
        assert!(contents.contains("retention = \"session\""), "the new key lands on disk");
    }

    #[test]
    fn toml_string_literal_quotes_and_escapes() {
        assert_eq!(Config::toml_string_literal("1_week"), "\"1_week\"");
        assert_eq!(Config::toml_string_literal("say \"hi\""), "\"say \\\"hi\\\"\"");
    }

    /// A throw-away temp directory unique to one test, so parallel tests
    /// never share files.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bolt-config-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }
}