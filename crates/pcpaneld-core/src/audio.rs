use serde::{Deserialize, Serialize};

/// Volume as a normalized value in [0.0, 1.0].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Volume(f64);

impl Volume {
    pub const ZERO: Volume = Volume(0.0);
    pub const MAX: Volume = Volume(1.0);

    /// Create a Volume from a normalized float, clamped to [0.0, 1.0].
    #[must_use]
    pub fn new(value: f64) -> Self {
        Volume(value.clamp(0.0, 1.0))
    }

    /// Get the raw normalized value.
    #[must_use]
    pub fn get(self) -> f64 {
        self.0
    }
}

/// Parameterized power curve for mapping hardware values to volume.
///
/// `volume = (hw_value / 255) ^ exponent`
///
/// Default exponent: 1.0 (linear). PulseAudio's volume scale already applies
/// perceptual (cubic) weighting, so a linear mapping here means slider
/// position corresponds directly to perceived volume percentage. An exponent
/// >1.0 adds extra resolution at the quiet end on top of PA's curve.
#[derive(Debug, Clone, Copy)]
pub struct VolumeCurve {
    exponent: f64,
}

impl Default for VolumeCurve {
    fn default() -> Self {
        VolumeCurve { exponent: 1.0 }
    }
}

impl VolumeCurve {
    /// Minimum exponent value. At 0.01, `(128/255)^0.01 ≈ 0.993` — aggressive
    /// but still has resolution across the range. Values at or below zero are
    /// clamped to this floor.
    pub const MIN_EXPONENT: f64 = 0.01;

    /// Create a volume curve with the given exponent, clamped to
    /// [`Self::MIN_EXPONENT`] if too small.
    ///
    /// PA's volume scale already applies perceptual weighting, so these values
    /// add *extra* shaping on top:
    /// - 1.0: linear (default) — slider position = PA volume percentage
    /// - 2.0: extra quiet-end resolution
    /// - 3.0: strong quiet-end bias
    #[must_use]
    pub fn new(exponent: f64) -> Self {
        VolumeCurve {
            exponent: exponent.max(Self::MIN_EXPONENT),
        }
    }

    /// Map a hardware value (0-255) to a normalized volume.
    #[must_use]
    pub fn hw_to_volume(&self, hw_value: u8) -> Volume {
        let normalized = f64::from(hw_value) / 255.0;
        Volume::new(normalized.powf(self.exponent))
    }

    /// Map a normalized volume back to the nearest hardware value (0-255).
    #[must_use]
    pub fn volume_to_hw(&self, volume: Volume) -> u8 {
        let normalized = volume.get().powf(1.0 / self.exponent);
        (normalized * 255.0).round().clamp(0.0, 255.0) as u8
    }

    #[must_use]
    pub fn exponent(&self) -> f64 {
        self.exponent
    }
}

/// Information about a PulseAudio sink (output device).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub volume: Volume,
    pub muted: bool,
    pub channels: u8,
}

/// Information about a PulseAudio source (input device).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub volume: Volume,
    pub muted: bool,
    pub channels: u8,
}

/// Information about a PulseAudio sink-input (application audio stream).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SinkInputInfo {
    pub index: u32,
    pub name: String,
    pub binary: Option<String>,
    pub flatpak_id: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    pub sink_index: u32,
    pub volume: Volume,
    pub muted: bool,
    pub channels: u8,
}

/// Full audio state snapshot from the PulseAudio thread.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioState {
    pub default_sink_name: Option<String>,
    pub default_source_name: Option<String>,
    pub sinks: Vec<SinkInfo>,
    pub sources: Vec<SourceInfo>,
    pub sink_inputs: Vec<SinkInputInfo>,
}

/// Type of audio device (output or input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceType {
    Output,
    Input,
}

/// Combined device info for CLI output (merges sinks and sources).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_type: DeviceType,
    pub index: u32,
    pub name: String,
    pub description: String,
    pub volume: Volume,
    pub muted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_clamped_to_valid_range() {
        assert_eq!(Volume::new(-0.5).get(), 0.0);
        assert_eq!(Volume::new(0.0).get(), 0.0);
        assert_eq!(Volume::new(0.5).get(), 0.5);
        assert_eq!(Volume::new(1.0).get(), 1.0);
        assert_eq!(Volume::new(1.5).get(), 1.0);
    }

    #[test]
    fn default_curve_endpoints() {
        let curve = VolumeCurve::default();
        assert_eq!(curve.hw_to_volume(0).get(), 0.0);
        assert!((curve.hw_to_volume(255).get() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn default_curve_midpoint_is_half() {
        let curve = VolumeCurve::default();
        let mid = curve.hw_to_volume(128).get();
        // Linear default: (128/255)^1.0 ≈ 0.502
        assert!(
            (mid - 128.0 / 255.0).abs() < 0.01,
            "midpoint volume {mid} should be ~0.502 with linear default"
        );
    }

    #[test]
    fn inverse_round_trip_within_epsilon() {
        let curve = VolumeCurve::default();
        for hw in 0..=255u8 {
            let vol = curve.hw_to_volume(hw);
            let back = curve.volume_to_hw(vol);
            assert!(
                (i16::from(hw) - i16::from(back)).unsigned_abs() <= 1,
                "hw={hw}, vol={}, back={back}",
                vol.get()
            );
        }
    }

    #[test]
    fn different_exponents_change_curve_shape() {
        let gentle = VolumeCurve::new(2.0);
        let standard = VolumeCurve::new(3.0);
        let aggressive = VolumeCurve::new(4.0);

        let hw = 128;
        let v_gentle = gentle.hw_to_volume(hw).get();
        let v_standard = standard.hw_to_volume(hw).get();
        let v_aggressive = aggressive.hw_to_volume(hw).get();

        // Higher exponent = quieter at midpoint
        assert!(v_gentle > v_standard);
        assert!(v_standard > v_aggressive);
    }

    #[test]
    fn full_sweep_is_monotonic() {
        let curve = VolumeCurve::default();
        let volumes: Vec<f64> = (0..=255).map(|hw| curve.hw_to_volume(hw).get()).collect();
        assert!(volumes.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn volume_always_in_valid_range() {
        for exp in [1.0, 2.0, 3.0, 4.0, 5.0] {
            let curve = VolumeCurve::new(exp);
            for hw in 0..=255u8 {
                let vol = curve.hw_to_volume(hw);
                assert!(vol.get() >= 0.0, "exp={exp}, hw={hw}: vol={}", vol.get());
                assert!(vol.get() <= 1.0, "exp={exp}, hw={hw}: vol={}", vol.get());
            }
        }
    }

    #[test]
    fn volume_curve_clamps_zero_exponent() {
        let curve = VolumeCurve::new(0.0);
        assert_eq!(curve.exponent(), VolumeCurve::MIN_EXPONENT);
    }

    #[test]
    fn volume_curve_clamps_negative_exponent() {
        let curve = VolumeCurve::new(-1.0);
        assert_eq!(curve.exponent(), VolumeCurve::MIN_EXPONENT);
    }

    #[test]
    fn volume_curve_clamped_still_produces_valid_output() {
        let curve = VolumeCurve::new(0.0);
        let volumes: Vec<f64> = (0..=255).map(|hw| curve.hw_to_volume(hw).get()).collect();
        // Endpoints
        assert_eq!(volumes[0], 0.0);
        assert!((volumes[255] - 1.0).abs() < f64::EPSILON);
        // Monotonicity
        assert!(volumes.windows(2).all(|w| w[1] >= w[0]));
        // All in valid range
        assert!(volumes.iter().all(|&v| (0.0..=1.0).contains(&v)));
    }

    #[test]
    fn volume_serde_round_trip() {
        let vol = Volume::new(0.42);
        let json = serde_json::to_string(&vol).unwrap();
        let parsed: Volume = serde_json::from_str(&json).unwrap();
        assert!((vol.get() - parsed.get()).abs() < f64::EPSILON);
    }

    #[test]
    fn sink_input_info_serde_with_pid() {
        let si = SinkInputInfo {
            index: 1,
            name: "Test".into(),
            binary: Some("test".into()),
            flatpak_id: None,
            pid: Some(12345),
            sink_index: 0,
            volume: Volume::new(0.5),
            muted: false,
            channels: 2,
        };
        let json = serde_json::to_string(&si).unwrap();
        let parsed: SinkInputInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pid, Some(12345));
    }

    #[test]
    fn sink_input_info_serde_missing_pid_defaults_to_none() {
        let json = r#"{
            "index": 1,
            "name": "Test",
            "binary": "test",
            "flatpak_id": null,
            "sink_index": 0,
            "volume": 0.5,
            "muted": false,
            "channels": 2
        }"#;
        let parsed: SinkInputInfo = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.pid, None);
    }
}
