use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::control::{ControlConfig, ControlId};

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("failed to read config from {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("failed to write config to {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("failed to parse config from {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml_edit::de::Error,
    },
    #[error("failed to serialize config: {source}")]
    Serialize { source: toml_edit::ser::Error },
    #[error("failed to create config directory {path}: {source}")]
    CreateDir { path: PathBuf, source: io::Error },
}

/// Device-specific configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceConfig {
    /// Optional serial number to lock to a specific device.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

/// Signal processing parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalConfig {
    #[serde(default = "default_slider_rolling_average")]
    pub slider_rolling_average: usize,
    #[serde(default = "default_slider_delta_threshold")]
    pub slider_delta_threshold: u8,
    #[serde(default = "default_slider_debounce_ms")]
    pub slider_debounce_ms: u64,
    #[serde(default = "default_knob_rolling_average")]
    pub knob_rolling_average: usize,
    #[serde(default = "default_knob_delta_threshold")]
    pub knob_delta_threshold: u8,
    #[serde(default = "default_knob_debounce_ms")]
    pub knob_debounce_ms: u64,
    #[serde(default = "default_volume_exponent")]
    pub volume_exponent: f64,
}

fn default_slider_rolling_average() -> usize {
    5
}
fn default_slider_delta_threshold() -> u8 {
    2
}
fn default_slider_debounce_ms() -> u64 {
    10
}
fn default_knob_rolling_average() -> usize {
    3
}
fn default_knob_delta_threshold() -> u8 {
    1
}
fn default_knob_debounce_ms() -> u64 {
    0
}
fn default_volume_exponent() -> f64 {
    1.0
}

fn default_true() -> bool {
    true
}

/// LED zone enable/disable configuration.
///
/// Controls which LED zones are active on the device. Disabled zones are sent
/// all-off commands. Default is all enabled (backward compatible).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedConfig {
    #[serde(default = "default_true")]
    pub knobs: bool,
    #[serde(default = "default_true")]
    pub sliders: bool,
    #[serde(default = "default_true")]
    pub slider_labels: bool,
    #[serde(default = "default_true")]
    pub logo: bool,
}

impl Default for LedConfig {
    fn default() -> Self {
        LedConfig {
            knobs: true,
            sliders: true,
            slider_labels: true,
            logo: true,
        }
    }
}

impl Default for SignalConfig {
    fn default() -> Self {
        SignalConfig {
            slider_rolling_average: default_slider_rolling_average(),
            slider_delta_threshold: default_slider_delta_threshold(),
            slider_debounce_ms: default_slider_debounce_ms(),
            knob_rolling_average: default_knob_rolling_average(),
            knob_delta_threshold: default_knob_delta_threshold(),
            knob_debounce_ms: default_knob_debounce_ms(),
            volume_exponent: default_volume_exponent(),
        }
    }
}

/// Top-level configuration.
///
/// Forward-compatible: unknown fields are silently ignored (no `deny_unknown_fields`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub device: DeviceConfig,
    #[serde(default)]
    pub signal: SignalConfig,
    #[serde(default)]
    pub leds: LedConfig,
    #[serde(default)]
    pub controls: HashMap<String, ControlConfig>,
}

impl Config {
    /// Get the configuration for a specific control, if any.
    #[must_use]
    pub fn get_control(&self, id: ControlId) -> Option<&ControlConfig> {
        self.controls.get(&id.config_key())
    }

    /// Set the configuration for a specific control.
    pub fn set_control(&mut self, id: ControlId, config: ControlConfig) {
        self.controls.insert(id.config_key(), config);
    }

    /// Remove the configuration for a specific control.
    pub fn remove_control(&mut self, id: ControlId) -> Option<ControlConfig> {
        self.controls.remove(&id.config_key())
    }

    /// Load config from a TOML file. Returns default config if file doesn't exist.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                toml_edit::de::from_str(&contents).map_err(|source| ConfigError::Parse {
                    path: path.to_owned(),
                    source,
                })
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.to_owned(),
                source,
            }),
        }
    }

    /// Serialize this config to a TOML string.
    ///
    /// Uses `toml_edit` to produce clean output with dotted keys for control
    /// sub-tables (e.g., `dial.type = "volume"`) instead of verbose nested
    /// table headers.
    pub fn to_toml(&self) -> Result<String, ConfigError> {
        let mut doc = toml_edit::ser::to_document(self)
            .map_err(|source| ConfigError::Serialize { source })?;

        // to_document() may produce InlineTable values for top-level sections.
        // Convert them to proper [section] tables for readable output.
        expand_top_level_tables(&mut doc);

        // Remove empty [controls] section (fresh config with no mappings)
        if self.controls.is_empty() {
            doc.remove("controls");
        }

        // Flatten nested control tables into dotted keys
        flatten_control_tables(&mut doc);

        Ok(doc.to_string())
    }

    /// Save config to a file, creating parent directories if needed.
    ///
    /// Uses atomic write: writes to `path.tmp` then renames over `path`.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| ConfigError::CreateDir {
                path: parent.to_owned(),
                source,
            })?;
        }

        let contents = self.to_toml()?;

        let tmp_path = path.with_extension("toml.tmp");
        std::fs::write(&tmp_path, &contents).map_err(|source| ConfigError::Write {
            path: tmp_path.clone(),
            source,
        })?;
        std::fs::rename(&tmp_path, path).map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })?;

        Ok(())
    }

    /// Returns the default config directory path.
    #[must_use]
    pub fn default_dir() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("pcpaneld"))
    }

    /// Returns the default config file path.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        Some(Self::default_dir()?.join("config.toml"))
    }
}

/// Convert top-level `InlineTable` values into proper `Table` entries so they
/// render as `[section]` headers instead of `section = { ... }` one-liners.
fn expand_top_level_tables(doc: &mut toml_edit::DocumentMut) {
    use toml_edit::{Item, Value};

    let keys: Vec<String> = doc.iter().map(|(k, _)| k.to_owned()).collect();
    for key in keys {
        let Some(item) = doc.get_mut(&key) else {
            continue;
        };
        if let Item::Value(Value::InlineTable(inline)) = item {
            let table = inline.clone().into_table();
            *item = Item::Table(table);
        }
    }
}

/// Mark sub-tables within each `[controls.<name>]` as dotted so they render as
/// `dial.type = "volume"` instead of `[controls.knob1.dial]\ntype = "volume"`.
fn flatten_control_tables(doc: &mut toml_edit::DocumentMut) {
    use toml_edit::{Item, Value};

    let Some(Item::Table(controls)) = doc.get_mut("controls") else {
        return;
    };

    // Convert any InlineTable control entries to proper Tables first
    let keys: Vec<String> = controls.iter().map(|(k, _)| k.to_owned()).collect();
    for key in &keys {
        let Some(item) = controls.get_mut(key) else {
            continue;
        };
        if let Item::Value(Value::InlineTable(inline)) = item {
            let table = inline.clone().into_table();
            *item = Item::Table(table);
        }
    }

    controls.sort_values();

    for (_, control_item) in controls.iter_mut() {
        if let Item::Table(control) = control_item {
            mark_subtables_dotted(control);
        }
    }
}

/// Recursively mark all nested tables as dotted.
///
/// Handles both `Item::Table` (normal sub-tables) and `Item::Value(InlineTable)`
/// which `to_document()` produces for `#[serde(tag = "type")]` tagged enums.
/// The latter is converted to a proper `Item::Table` marked dotted.
fn mark_subtables_dotted(table: &mut toml_edit::Table) {
    use toml_edit::{Item, Value};

    // Collect keys first to avoid borrow issues
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_owned()).collect();

    for key in keys {
        let Some(item) = table.get_mut(&key) else {
            continue;
        };
        match item {
            Item::Table(sub) => {
                sub.set_dotted(true);
                mark_subtables_dotted(sub);
            }
            Item::Value(Value::InlineTable(inline)) => {
                let mut sub = inline.clone().into_table();
                sub.set_dotted(true);
                mark_subtables_dotted(&mut sub);
                *item = Item::Table(sub);
            }
            _ => {}
        }
    }
}

const HEADER: &str = "\
# PCPanel Pro configuration
# Changes are auto-detected — no manual reload needed.
# Full reference: docs/configuration.md

";

const EXAMPLE: &str = "\n\
# Example: control an app's volume by binary name
# [controls.knob3]
# dial.type = \"volume\"
# dial.target.type = \"app\"
# dial.target.matcher.binary = \"firefox\"
# button.type = \"mute\"
# button.target.type = \"app\"
# button.target.matcher.binary = \"firefox\"

# Example: media control button (play/pause, next, previous, stop, play, pause)
# [controls.knob4]
# button.type = \"media\"
# button.command = \"play_pause\"

# Example: run a shell command on button press
# [controls.knob5]
# button.type = \"exec\"
# button.command = \"notify-send 'Button pressed!'\"

# Example: disable LEDs (all zones default to true if omitted)
# [leds]
# knobs = false
# sliders = false
# slider_labels = false
# logo = false
";

/// Generate the default config file content for new users.
///
/// Builds a starter config from `Config::default()` with knob1 (default output)
/// and knob2 (default input) pre-configured. The output is valid TOML
/// generated from the same serialization path as `Config::save()`, so defaults
/// can never drift from the code.
pub fn default_config_content() -> Result<String, ConfigError> {
    use crate::control::{AudioTarget, ButtonAction, DialAction};

    let mut config = Config::default();
    config.set_control(
        ControlId::Knob(0),
        ControlConfig {
            dial: Some(DialAction::Volume {
                target: AudioTarget::DefaultOutput,
            }),
            button: Some(ButtonAction::Mute {
                target: AudioTarget::DefaultOutput,
            }),
        },
    );
    config.set_control(
        ControlId::Knob(1),
        ControlConfig {
            dial: Some(DialAction::Volume {
                target: AudioTarget::DefaultInput,
            }),
            button: Some(ButtonAction::Mute {
                target: AudioTarget::DefaultInput,
            }),
        },
    );

    let body = config.to_toml()?;

    Ok(format!("{HEADER}{body}{EXAMPLE}"))
}

/// Write the default config with comments to the given path if it doesn't exist.
/// Creates parent directories as needed. Returns true if the file was created.
pub fn bootstrap_config(path: &Path) -> Result<bool, ConfigError> {
    if path.exists() {
        return Ok(false);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::CreateDir {
            path: parent.to_owned(),
            source,
        })?;
    }

    std::fs::write(path, default_config_content()?).map_err(|source| ConfigError::Write {
        path: path.to_owned(),
        source,
    })?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::*;

    #[test]
    fn default_config_is_valid() {
        let config = Config::default();
        assert_eq!(config.signal.volume_exponent, 1.0);
        assert_eq!(config.signal.slider_rolling_average, 5);
        assert!(config.controls.is_empty());
    }

    #[test]
    fn empty_toml_uses_defaults() {
        let config: Config = toml_edit::de::from_str("").unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn partial_toml_preserves_defaults() {
        let config: Config = toml_edit::de::from_str(
            r#"
            [signal]
            volume_exponent = 2.0
            "#,
        )
        .unwrap();

        assert_eq!(config.signal.volume_exponent, 2.0);
        // Other fields should have defaults
        assert_eq!(config.signal.slider_rolling_average, 5);
        assert_eq!(config.signal.knob_delta_threshold, 1);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let config: Config = toml_edit::de::from_str(
            r#"
            [signal]
            volume_exponent = 2.0
            future_field = "hello"
            another_thing = 42
            "#,
        )
        .unwrap();
        assert_eq!(config.signal.volume_exponent, 2.0);
    }

    #[test]
    fn unknown_top_level_sections_ignored() {
        let config: Config = toml_edit::de::from_str(
            r#"
            [device]
            serial = "ABC123"

            [some_future_section]
            key = "value"
            "#,
        )
        .unwrap();
        assert_eq!(config.device.serial.as_deref(), Some("ABC123"));
    }

    #[test]
    fn control_config_round_trip() {
        let mut config = Config::default();
        config.set_control(
            ControlId::Knob(0),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultOutput,
                }),
                button: Some(ButtonAction::Mute {
                    target: AudioTarget::DefaultOutput,
                }),
            },
        );
        config.set_control(
            ControlId::Slider(0),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::App {
                        matcher: AppMatcher {
                            binary: Some("firefox".into()),
                            ..Default::default()
                        },
                    },
                }),
                button: None,
            },
        );

        let toml_str = config.to_toml().unwrap();
        let parsed: Config = toml_edit::de::from_str(&toml_str).unwrap();

        assert_eq!(
            config.get_control(ControlId::Knob(0)),
            parsed.get_control(ControlId::Knob(0))
        );
        assert_eq!(
            config.get_control(ControlId::Slider(0)),
            parsed.get_control(ControlId::Slider(0))
        );
    }

    #[test]
    fn default_config_content_round_trips() {
        let content = default_config_content().unwrap();
        let parsed: Config = toml_edit::de::from_str(&content).unwrap();
        assert!(parsed.device.serial.is_none());
        assert_eq!(parsed.signal, Config::default().signal);
        assert!(parsed.get_control(ControlId::Knob(0)).is_some());
        assert!(parsed.get_control(ControlId::Knob(1)).is_some());
        assert_eq!(parsed.controls.len(), 2);
    }

    #[test]
    fn malformed_toml_returns_parse_error() {
        let result: Result<Config, _> = toml_edit::de::from_str("this is not valid [toml");
        assert!(result.is_err());
    }

    #[test]
    fn load_nonexistent_returns_defaults() {
        let config = Config::load(Path::new("/nonexistent/path/config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn set_and_remove_control() {
        let mut config = Config::default();
        let id = ControlId::Knob(2);
        assert!(config.get_control(id).is_none());

        config.set_control(
            id,
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultOutput,
                }),
                button: None,
            },
        );
        assert!(config.get_control(id).is_some());

        let removed = config.remove_control(id);
        assert!(removed.is_some());
        assert!(config.get_control(id).is_none());
    }

    #[test]
    fn app_target_config_round_trip() {
        let toml_str = r#"
        [controls.knob3]
        dial = { type = "volume", target = { type = "app", matcher = { flatpak_id = "org.mozilla.firefox" } } }
        button = { type = "mute", target = { type = "app", matcher = { flatpak_id = "org.mozilla.firefox" } } }
        "#;

        let config: Config = toml_edit::de::from_str(toml_str).unwrap();
        let ctrl = config.get_control(ControlId::Knob(2)).unwrap();
        assert!(ctrl.dial.is_some());
        assert!(ctrl.button.is_some());

        if let Some(DialAction::Volume {
            target: AudioTarget::App { matcher },
        }) = &ctrl.dial
        {
            assert_eq!(matcher.flatpak_id.as_deref(), Some("org.mozilla.firefox"));
        } else {
            panic!("expected app volume dial action");
        }
    }

    #[test]
    fn focused_app_target_config_round_trip() {
        let toml_str = r#"
        [controls.slider4]
        dial = { type = "volume", target = { type = "focused_app" } }
        "#;

        let config: Config = toml_edit::de::from_str(toml_str).unwrap();
        let ctrl = config.get_control(ControlId::Slider(3)).unwrap();
        assert!(ctrl.dial.is_some());

        if let Some(DialAction::Volume {
            target: AudioTarget::FocusedApp,
        }) = &ctrl.dial
        {
            // expected
        } else {
            panic!("expected focused_app volume dial action");
        }

        // Round-trip through serialization
        let serialized = config.to_toml().unwrap();
        let reparsed: Config = toml_edit::de::from_str(&serialized).unwrap();
        assert_eq!(
            config.get_control(ControlId::Slider(3)),
            reparsed.get_control(ControlId::Slider(3))
        );
    }

    #[test]
    fn save_and_load_round_trip_via_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = Config::default();
        config.device.serial = Some("TEST123".into());
        config.set_control(
            ControlId::Knob(0),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultOutput,
                }),
                button: None,
            },
        );

        config.save(&path).unwrap();
        let loaded = Config::load(&path).unwrap();

        assert_eq!(config, loaded);
    }

    #[test]
    fn full_example_config_parses() {
        let toml_str = r#"
[device]
serial = "5D43E1353833"

[signal]
slider_rolling_average = 5
slider_delta_threshold = 2
slider_debounce_ms = 10
knob_rolling_average = 3
knob_delta_threshold = 1
knob_debounce_ms = 0
volume_exponent = 3.0

[controls.knob1]
dial = { type = "volume", target = { type = "default_sink" } }
button = { type = "mute", target = { type = "default_sink" } }

[controls.knob2]
dial = { type = "volume", target = { type = "default_source" } }
button = { type = "mute", target = { type = "default_source" } }

[controls.knob3]
dial = { type = "volume", target = { type = "app", matcher = { flatpak_id = "org.mozilla.firefox" } } }
button = { type = "mute", target = { type = "app", matcher = { flatpak_id = "org.mozilla.firefox" } } }

[controls.knob4]
dial = { type = "volume", target = { type = "app", matcher = { binary = "spotify" } } }

[controls.slider1]
dial = { type = "volume", target = { type = "app", matcher = { name = "Steam" } } }
        "#;

        let config: Config = toml_edit::de::from_str(toml_str).unwrap();
        assert_eq!(config.device.serial.as_deref(), Some("5D43E1353833"));
        assert!(config.get_control(ControlId::Knob(0)).is_some());
        assert!(config.get_control(ControlId::Knob(1)).is_some());
        assert!(config.get_control(ControlId::Knob(2)).is_some());
        assert!(config.get_control(ControlId::Knob(3)).is_some());
        assert!(config.get_control(ControlId::Slider(0)).is_some());
    }

    #[test]
    fn to_toml_produces_dotted_keys() {
        let mut config = Config::default();
        config.set_control(
            ControlId::Knob(0),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultOutput,
                }),
                button: Some(ButtonAction::Mute {
                    target: AudioTarget::DefaultOutput,
                }),
            },
        );

        let output = config.to_toml().unwrap();

        // Should use dotted keys within control sections
        assert!(
            output.contains("dial.type"),
            "expected dial.type dotted key"
        );
        assert!(
            output.contains("dial.target.type"),
            "expected dial.target.type dotted key"
        );
        assert!(
            output.contains("button.type"),
            "expected button.type dotted key"
        );

        // Should NOT have deeply nested table headers
        assert!(
            !output.contains("[controls.knob1.dial]"),
            "should not have nested [controls.knob1.dial] header"
        );
        assert!(
            !output.contains("[controls.knob1.button]"),
            "should not have nested [controls.knob1.button] header"
        );

        // Round-trip: dotted output must parse back identically
        let parsed: Config = toml_edit::de::from_str(&output).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn to_toml_keeps_default_sections() {
        let config = Config::default();
        let output = config.to_toml().unwrap();

        assert!(output.contains("[device]"), "expected [device] section");
        assert!(output.contains("[signal]"), "expected [signal] section");
        assert!(
            !output.contains("[controls]"),
            "empty controls should be suppressed"
        );
    }

    #[test]
    fn to_toml_app_target_produces_dotted_matcher() {
        let mut config = Config::default();
        config.set_control(
            ControlId::Knob(2),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::App {
                        matcher: AppMatcher {
                            binary: Some("firefox-bin".into()),
                            ..Default::default()
                        },
                    },
                }),
                button: None,
            },
        );

        let output = config.to_toml().unwrap();

        assert!(
            output.contains("dial.target.matcher.binary"),
            "expected dotted matcher path, got:\n{output}"
        );

        // Round-trip
        let parsed: Config = toml_edit::de::from_str(&output).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn to_toml_controls_sorted() {
        let mut config = Config::default();

        // Insert in non-sorted order
        config.set_control(
            ControlId::Slider(0),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultOutput,
                }),
                button: None,
            },
        );
        config.set_control(
            ControlId::Knob(0),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultOutput,
                }),
                button: None,
            },
        );
        config.set_control(
            ControlId::Knob(1),
            ControlConfig {
                dial: Some(DialAction::Volume {
                    target: AudioTarget::DefaultInput,
                }),
                button: None,
            },
        );

        let output = config.to_toml().unwrap();

        let knob1_pos = output.find("[controls.knob1]").expect("knob1 present");
        let knob2_pos = output.find("[controls.knob2]").expect("knob2 present");
        let slider1_pos = output.find("[controls.slider1]").expect("slider1 present");

        assert!(knob1_pos < knob2_pos, "knob1 should come before knob2");
        assert!(knob2_pos < slider1_pos, "knob2 should come before slider1");
    }

    #[test]
    fn led_config_default_is_all_enabled() {
        let config = LedConfig::default();
        assert!(config.knobs);
        assert!(config.sliders);
        assert!(config.slider_labels);
        assert!(config.logo);
    }

    #[test]
    fn led_config_partial_toml_preserves_defaults() {
        let config: Config = toml_edit::de::from_str(
            r#"
            [leds]
            knobs = false
            "#,
        )
        .unwrap();
        assert!(!config.leds.knobs);
        assert!(config.leds.sliders);
        assert!(config.leds.slider_labels);
        assert!(config.leds.logo);
    }

    #[test]
    fn led_config_missing_section_uses_defaults() {
        let config: Config = toml_edit::de::from_str("").unwrap();
        assert_eq!(config.leds, LedConfig::default());
    }

    #[test]
    fn led_config_round_trip() {
        let mut config = Config::default();
        config.leds.knobs = false;
        config.leds.logo = false;
        let toml_str = config.to_toml().unwrap();
        let parsed: Config = toml_edit::de::from_str(&toml_str).unwrap();
        assert_eq!(config.leds, parsed.leds);
    }

    #[test]
    fn led_config_unknown_fields_ignored() {
        let config: Config = toml_edit::de::from_str(
            r#"
            [leds]
            knobs = false
            future_field = true
            "#,
        )
        .unwrap();
        assert!(!config.leds.knobs);
    }

    #[test]
    fn to_toml_includes_leds_section() {
        let config = Config::default();
        let output = config.to_toml().unwrap();
        assert!(
            output.contains("[leds]"),
            "expected [leds] section in output"
        );
    }

    #[test]
    fn exec_button_config_toml_dotted_keys() {
        let mut config = Config::default();
        config.set_control(
            ControlId::Knob(2),
            ControlConfig {
                dial: None,
                button: Some(ButtonAction::Exec {
                    command: "notify-send hello".into(),
                }),
            },
        );

        let output = config.to_toml().unwrap();
        assert!(
            output.contains("button.type"),
            "expected button.type dotted key, got:\n{output}"
        );
        assert!(
            output.contains("button.command"),
            "expected button.command dotted key, got:\n{output}"
        );

        // Round-trip
        let parsed: Config = toml_edit::de::from_str(&output).unwrap();
        assert_eq!(
            config.get_control(ControlId::Knob(2)),
            parsed.get_control(ControlId::Knob(2))
        );
    }

    #[test]
    fn media_button_config_toml_dotted_keys() {
        use crate::control::MediaCommand;

        let mut config = Config::default();
        config.set_control(
            ControlId::Knob(3),
            ControlConfig {
                dial: None,
                button: Some(ButtonAction::Media {
                    command: MediaCommand::PlayPause,
                }),
            },
        );

        let output = config.to_toml().unwrap();
        assert!(
            output.contains("button.type"),
            "expected button.type dotted key, got:\n{output}"
        );
        assert!(
            output.contains("button.command"),
            "expected button.command dotted key, got:\n{output}"
        );

        // Round-trip
        let parsed: Config = toml_edit::de::from_str(&output).unwrap();
        assert_eq!(
            config.get_control(ControlId::Knob(3)),
            parsed.get_control(ControlId::Knob(3))
        );
    }
}
