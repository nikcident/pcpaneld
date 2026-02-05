use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifies a physical control on the PCPanel Pro.
///
/// Knobs 0-4 (rotary encoders with buttons), Sliders 0-3 (linear faders).
/// Each knob has both an analog dial and a digital button; sliders are analog only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ControlId {
    Knob(u8),
    Slider(u8),
}

impl ControlId {
    /// Number of knobs on the PCPanel Pro.
    pub const NUM_KNOBS: u8 = 5;
    /// Number of sliders on the PCPanel Pro.
    pub const NUM_SLIDERS: u8 = 4;
    /// Total number of analog controls.
    pub const NUM_ANALOG: u8 = Self::NUM_KNOBS + Self::NUM_SLIDERS;

    /// Convert a HID analog control ID (0-8) to a `ControlId`.
    /// IDs 0-4 are knobs, 5-8 are sliders.
    #[must_use]
    pub fn from_analog_id(id: u8) -> Option<Self> {
        if id < Self::NUM_KNOBS {
            Some(ControlId::Knob(id))
        } else if id < Self::NUM_ANALOG {
            Some(ControlId::Slider(id - Self::NUM_KNOBS))
        } else {
            None
        }
    }

    /// Convert back to a HID analog control ID (0-8).
    #[must_use]
    pub fn to_analog_id(self) -> u8 {
        match self {
            ControlId::Knob(n) => n,
            ControlId::Slider(n) => Self::NUM_KNOBS + n,
        }
    }

    /// Convert a HID button ID (0-4) to a `ControlId`.
    /// Only knobs have buttons on the PCPanel Pro.
    #[must_use]
    pub fn from_button_id(id: u8) -> Option<Self> {
        if id < Self::NUM_KNOBS {
            Some(ControlId::Knob(id))
        } else {
            None
        }
    }

    /// Returns the config key name for this control (e.g., "knob1", "slider2").
    /// Uses 1-based indexing for human readability.
    #[must_use]
    pub fn config_key(&self) -> String {
        match self {
            ControlId::Knob(n) => format!("knob{}", n + 1),
            ControlId::Slider(n) => format!("slider{}", n + 1),
        }
    }

    /// Parse a config key name back to a `ControlId`.
    /// Accepts "knob1"-"knob5" and "slider1"-"slider4" (1-based).
    #[must_use]
    pub fn from_config_key(key: &str) -> Option<Self> {
        if let Some(n) = key.strip_prefix("knob") {
            let n: u8 = n.parse().ok()?;
            if (1..=Self::NUM_KNOBS).contains(&n) {
                return Some(ControlId::Knob(n - 1));
            }
        } else if let Some(n) = key.strip_prefix("slider") {
            let n: u8 = n.parse().ok()?;
            if (1..=Self::NUM_SLIDERS).contains(&n) {
                return Some(ControlId::Slider(n - 1));
            }
        }
        None
    }

    /// Returns true if this control is a knob (has both dial and button).
    #[must_use]
    pub fn is_knob(self) -> bool {
        matches!(self, ControlId::Knob(_))
    }

    /// Returns true if this control is a slider (analog only).
    #[must_use]
    pub fn is_slider(self) -> bool {
        matches!(self, ControlId::Slider(_))
    }
}

/// Matches PulseAudio sink-inputs by application properties.
///
/// When multiple fields are set, ALL must match (AND logic).
/// Each field uses case-insensitive substring matching.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppMatcher {
    /// Match against `application.process.binary`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<String>,
    /// Match against `application.name`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Match against `application.flatpak.id`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flatpak_id: Option<String>,
}

impl AppMatcher {
    /// Returns true if this matcher has at least one field set.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.binary.is_some() || self.name.is_some() || self.flatpak_id.is_some()
    }

    /// Check if a sink-input's properties match this matcher.
    ///
    /// Uses case-insensitive substring matching. When multiple fields are
    /// specified, ALL must match (AND logic). An empty matcher matches nothing.
    #[must_use]
    pub fn matches(&self, props: &AppProperties) -> bool {
        if !self.is_valid() {
            return false;
        }

        let check = |pattern: &Option<String>, value: &Option<String>| -> bool {
            match (pattern, value) {
                (Some(pat), Some(val)) => val.to_lowercase().contains(&pat.to_lowercase()),
                (Some(_), None) => false,
                (None, _) => true, // field not specified in matcher, skip
            }
        };

        check(&self.binary, &props.binary)
            && check(&self.name, &props.name)
            && check(&self.flatpak_id, &props.flatpak_id)
    }
}

/// Properties of a PulseAudio sink-input used for app matching.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppProperties {
    pub binary: Option<String>,
    pub name: Option<String>,
    pub flatpak_id: Option<String>,
}

impl From<&crate::audio::SinkInputInfo> for AppProperties {
    fn from(si: &crate::audio::SinkInputInfo) -> Self {
        AppProperties {
            binary: si.binary.clone(),
            name: Some(si.name.clone()),
            flatpak_id: si.flatpak_id.clone(),
        }
    }
}

/// The target for a volume/mute action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AudioTarget {
    /// The default audio output device.
    #[serde(alias = "default_sink")]
    DefaultOutput,
    /// The default audio input device.
    #[serde(alias = "default_source")]
    DefaultInput,
    /// A specific application matched by properties.
    App { matcher: AppMatcher },
    /// The currently focused window's application.
    FocusedApp,
}

impl fmt::Display for AudioTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioTarget::DefaultOutput => f.write_str("default-output"),
            AudioTarget::DefaultInput => f.write_str("default-input"),
            AudioTarget::App { matcher } => {
                let mut parts = Vec::new();
                if let Some(b) = &matcher.binary {
                    parts.push(format!("binary={b}"));
                }
                if let Some(n) = &matcher.name {
                    parts.push(format!("name={n}"));
                }
                if let Some(fid) = &matcher.flatpak_id {
                    parts.push(format!("flatpak={fid}"));
                }
                write!(f, "app({})", parts.join(", "))
            }
            AudioTarget::FocusedApp => f.write_str("focused"),
        }
    }
}

/// Action for a dial (knob rotation or slider movement).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DialAction {
    Volume { target: AudioTarget },
}

/// MPRIS media player command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaCommand {
    PlayPause,
    Play,
    Pause,
    Next,
    Previous,
    Stop,
}

impl MediaCommand {
    /// Returns the MPRIS D-Bus method name for this command.
    #[must_use]
    pub fn method_name(self) -> &'static str {
        match self {
            MediaCommand::PlayPause => "PlayPause",
            MediaCommand::Play => "Play",
            MediaCommand::Pause => "Pause",
            MediaCommand::Next => "Next",
            MediaCommand::Previous => "Previous",
            MediaCommand::Stop => "Stop",
        }
    }
}

/// Action for a button press.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ButtonAction {
    Mute { target: AudioTarget },
    Media { command: MediaCommand },
    Exec { command: String },
}

/// Configuration for a single physical control.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dial: Option<DialAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub button: Option<ButtonAction>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analog_id_round_trip() {
        for id in 0..ControlId::NUM_ANALOG {
            let control = ControlId::from_analog_id(id).unwrap();
            assert_eq!(control.to_analog_id(), id);
        }
    }

    #[test]
    fn analog_id_out_of_range() {
        assert_eq!(ControlId::from_analog_id(9), None);
        assert_eq!(ControlId::from_analog_id(255), None);
    }

    #[test]
    fn button_id_valid_range() {
        for id in 0..5 {
            let control = ControlId::from_button_id(id).unwrap();
            assert!(control.is_knob());
        }
    }

    #[test]
    fn button_id_out_of_range() {
        assert_eq!(ControlId::from_button_id(5), None);
        assert_eq!(ControlId::from_button_id(255), None);
    }

    #[test]
    fn config_key_round_trip() {
        for id in 0..ControlId::NUM_ANALOG {
            let control = ControlId::from_analog_id(id).unwrap();
            let key = control.config_key();
            let parsed = ControlId::from_config_key(&key).unwrap();
            assert_eq!(control, parsed);
        }
    }

    #[test]
    fn config_key_invalid() {
        assert_eq!(ControlId::from_config_key("knob0"), None);
        assert_eq!(ControlId::from_config_key("knob6"), None);
        assert_eq!(ControlId::from_config_key("slider0"), None);
        assert_eq!(ControlId::from_config_key("slider5"), None);
        assert_eq!(ControlId::from_config_key("fader1"), None);
        assert_eq!(ControlId::from_config_key(""), None);
        assert_eq!(ControlId::from_config_key("knob"), None);
    }

    #[test]
    fn knob_ids_are_0_to_4() {
        for id in 0..5u8 {
            let control = ControlId::from_analog_id(id).unwrap();
            assert_eq!(control, ControlId::Knob(id));
            assert!(control.is_knob());
            assert!(!control.is_slider());
        }
    }

    #[test]
    fn slider_ids_are_5_to_8() {
        for id in 5..9u8 {
            let control = ControlId::from_analog_id(id).unwrap();
            assert_eq!(control, ControlId::Slider(id - 5));
            assert!(control.is_slider());
            assert!(!control.is_knob());
        }
    }

    #[test]
    fn app_properties_from_sink_input_info() {
        use crate::audio::{SinkInputInfo, Volume};
        let si = SinkInputInfo {
            index: 1,
            name: "Firefox".into(),
            binary: Some("firefox".into()),
            flatpak_id: Some("org.mozilla.firefox".into()),
            pid: Some(1234),
            sink_index: 0,
            volume: Volume::new(0.5),
            muted: false,
            channels: 2,
        };
        let props = AppProperties::from(&si);
        assert_eq!(props.binary, Some("firefox".into()));
        assert_eq!(props.name, Some("Firefox".into()));
        assert_eq!(props.flatpak_id, Some("org.mozilla.firefox".into()));
    }

    #[test]
    fn empty_matcher_matches_nothing() {
        let matcher = AppMatcher::default();
        let props = AppProperties {
            binary: Some("firefox".into()),
            name: Some("Firefox".into()),
            flatpak_id: None,
        };
        assert!(!matcher.matches(&props));
        assert!(!matcher.is_valid());
    }

    #[test]
    fn binary_match_case_insensitive_substring() {
        let matcher = AppMatcher {
            binary: Some("fire".into()),
            ..Default::default()
        };

        assert!(matcher.matches(&AppProperties {
            binary: Some("firefox".into()),
            ..Default::default()
        }));
        assert!(matcher.matches(&AppProperties {
            binary: Some("Firefox".into()),
            ..Default::default()
        }));
        assert!(matcher.matches(&AppProperties {
            binary: Some("FIREFOX-BIN".into()),
            ..Default::default()
        }));
        assert!(!matcher.matches(&AppProperties {
            binary: Some("chrome".into()),
            ..Default::default()
        }));
        assert!(!matcher.matches(&AppProperties {
            binary: None,
            ..Default::default()
        }));
    }

    #[test]
    fn flatpak_id_match() {
        let matcher = AppMatcher {
            flatpak_id: Some("org.mozilla.firefox".into()),
            ..Default::default()
        };
        assert!(matcher.matches(&AppProperties {
            flatpak_id: Some("org.mozilla.firefox".into()),
            ..Default::default()
        }));
        assert!(matcher.matches(&AppProperties {
            flatpak_id: Some("org.mozilla.Firefox".into()),
            ..Default::default()
        }));
        assert!(!matcher.matches(&AppProperties {
            flatpak_id: None,
            ..Default::default()
        }));
    }

    #[test]
    fn and_logic_multiple_fields() {
        let matcher = AppMatcher {
            binary: Some("firefox".into()),
            name: Some("Firefox".into()),
            ..Default::default()
        };

        // Both match
        assert!(matcher.matches(&AppProperties {
            binary: Some("firefox".into()),
            name: Some("Firefox Web Browser".into()),
            ..Default::default()
        }));

        // Only binary matches
        assert!(!matcher.matches(&AppProperties {
            binary: Some("firefox".into()),
            name: Some("Chrome".into()),
            ..Default::default()
        }));

        // Only name matches
        assert!(!matcher.matches(&AppProperties {
            binary: Some("chrome".into()),
            name: Some("Firefox".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn name_match_case_insensitive_substring() {
        let matcher = AppMatcher {
            name: Some("Fire".into()),
            ..Default::default()
        };
        let yes = AppProperties {
            binary: Some("firefox".into()),
            name: Some("Firefox".into()),
            flatpak_id: None,
        };
        let no = AppProperties {
            binary: Some("chrome".into()),
            name: Some("Chrome".into()),
            flatpak_id: None,
        };
        assert!(matcher.matches(&yes));
        assert!(!matcher.matches(&no));
    }

    #[test]
    fn audio_target_serialization() {
        let target = AudioTarget::App {
            matcher: AppMatcher {
                binary: Some("firefox".into()),
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&target).unwrap();
        let parsed: AudioTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(target, parsed);
    }

    #[test]
    fn focused_app_json_round_trip() {
        let target = AudioTarget::FocusedApp;
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, r#"{"type":"focused_app"}"#);
        let parsed: AudioTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(target, parsed);
    }

    #[test]
    fn focused_app_toml_round_trip() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Wrapper {
            target: AudioTarget,
        }

        let w = Wrapper {
            target: AudioTarget::FocusedApp,
        };
        let toml_str = toml_edit::ser::to_string(&w).unwrap();
        let parsed: Wrapper = toml_edit::de::from_str(&toml_str).unwrap();
        assert_eq!(w, parsed);
    }

    #[test]
    fn control_config_serde_round_trip() {
        let config = ControlConfig {
            dial: Some(DialAction::Volume {
                target: AudioTarget::DefaultOutput,
            }),
            button: Some(ButtonAction::Mute {
                target: AudioTarget::DefaultOutput,
            }),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: ControlConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn button_action_media_json_round_trip() {
        let action = ButtonAction::Media {
            command: MediaCommand::PlayPause,
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: ButtonAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, parsed);
    }

    #[test]
    fn button_action_exec_json_round_trip() {
        let action = ButtonAction::Exec {
            command: "notify-send 'hello world'".into(),
        };
        let json = serde_json::to_string(&action).unwrap();
        let parsed: ButtonAction = serde_json::from_str(&json).unwrap();
        assert_eq!(action, parsed);
    }

    #[test]
    fn media_command_all_variants_round_trip() {
        let variants = [
            MediaCommand::PlayPause,
            MediaCommand::Play,
            MediaCommand::Pause,
            MediaCommand::Next,
            MediaCommand::Previous,
            MediaCommand::Stop,
        ];
        for cmd in &variants {
            let json = serde_json::to_string(cmd).unwrap();
            let parsed: MediaCommand = serde_json::from_str(&json).unwrap();
            assert_eq!(*cmd, parsed);
        }
    }

    #[test]
    fn media_command_method_names() {
        assert_eq!(MediaCommand::PlayPause.method_name(), "PlayPause");
        assert_eq!(MediaCommand::Play.method_name(), "Play");
        assert_eq!(MediaCommand::Pause.method_name(), "Pause");
        assert_eq!(MediaCommand::Next.method_name(), "Next");
        assert_eq!(MediaCommand::Previous.method_name(), "Previous");
        assert_eq!(MediaCommand::Stop.method_name(), "Stop");
    }

    #[test]
    fn button_action_media_toml_round_trip() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Wrapper {
            button: ButtonAction,
        }

        let w = Wrapper {
            button: ButtonAction::Media {
                command: MediaCommand::PlayPause,
            },
        };
        let toml_str = toml_edit::ser::to_string(&w).unwrap();
        let parsed: Wrapper = toml_edit::de::from_str(&toml_str).unwrap();
        assert_eq!(w, parsed);
    }

    #[test]
    fn button_action_exec_toml_round_trip() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct Wrapper {
            button: ButtonAction,
        }

        let w = Wrapper {
            button: ButtonAction::Exec {
                command: "notify-send 'Button pressed!'".into(),
            },
        };
        let toml_str = toml_edit::ser::to_string(&w).unwrap();
        let parsed: Wrapper = toml_edit::de::from_str(&toml_str).unwrap();
        assert_eq!(w, parsed);
    }
}
