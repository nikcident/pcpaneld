use thiserror::Error;

/// PCPanel Pro USB identifiers.
pub const VENDOR_ID: u16 = 0x0483;
pub const PRODUCT_ID: u16 = 0xA3C5;

/// HID report payload size (excluding Report ID byte).
pub const REPORT_SIZE: usize = 64;

#[derive(Error, Debug)]
pub enum HidError {
    #[error("HID device I/O error: {0}")]
    Io(String),
    #[error("HID device not found (VID={vid:#06x}, PID={pid:#06x})")]
    DeviceNotFound { vid: u16, pid: u16 },
    #[error("invalid HID report: {0}")]
    InvalidReport(String),
    #[error("HID write failed: expected {expected} bytes, wrote {actual}")]
    ShortWrite { expected: usize, actual: usize },
}

/// A parsed HID input event from the PCPanel Pro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidEvent {
    /// Knob or slider position change.
    /// `control_id`: 0-4 = knobs, 5-8 = sliders.
    /// `value`: 0-255 analog position.
    Position { control_id: u8, value: u8 },
    /// Button press or release.
    /// `button_id`: 0-4 (corresponding to knobs 0-4).
    Button { button_id: u8, pressed: bool },
}

impl HidEvent {
    /// Parse a raw HID input report into an event.
    ///
    /// The report should NOT include the Report ID byte (kernel strips it on reads).
    /// Expects at least 3 bytes: `[type, id, value]`.
    #[must_use = "parsing may return an error that should be handled"]
    pub fn parse(report: &[u8]) -> Result<Self, HidError> {
        if report.len() < 3 {
            return Err(HidError::InvalidReport(format!(
                "report too short: {} bytes, need at least 3",
                report.len()
            )));
        }

        match report[0] {
            0x01 => {
                let control_id = report[1];
                if control_id > 8 {
                    return Err(HidError::InvalidReport(format!(
                        "invalid analog control ID: {control_id} (expected 0-8)"
                    )));
                }
                Ok(HidEvent::Position {
                    control_id,
                    value: report[2],
                })
            }
            0x02 => {
                let button_id = report[1];
                if button_id > 4 {
                    return Err(HidError::InvalidReport(format!(
                        "invalid button ID: {button_id} (expected 0-4)"
                    )));
                }
                let pressed = match report[2] {
                    0 => false,
                    1 => true,
                    v => {
                        return Err(HidError::InvalidReport(format!(
                            "invalid button state: {v} (expected 0 or 1)"
                        )));
                    }
                };
                Ok(HidEvent::Button { button_id, pressed })
            }
            t => Err(HidError::InvalidReport(format!(
                "unknown report type: {t:#04x} (expected 0x01 or 0x02)"
            ))),
        }
    }
}

/// LED mode for a single element (knob ring or slider).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LedMode {
    Off = 0,
    Static = 1,
    Gradient = 2,
    VolumeGradient = 3,
}

/// LED configuration for a single element (7-byte slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LedSlot {
    pub mode: LedMode,
    pub r1: u8,
    pub g1: u8,
    pub b1: u8,
    pub r2: u8,
    pub g2: u8,
    pub b2: u8,
}

impl LedSlot {
    /// LED slot that produces no visible light.
    ///
    /// Uses `LedMode::Static` with black `(0,0,0)` rather than `LedMode::Off`
    /// because the PCPanel Pro firmware treats mode byte `0x00` as "no change."
    pub const OFF: LedSlot = LedSlot {
        mode: LedMode::Static,
        r1: 0,
        g1: 0,
        b1: 0,
        r2: 0,
        g2: 0,
        b2: 0,
    };

    #[must_use]
    pub fn static_color(r: u8, g: u8, b: u8) -> Self {
        LedSlot {
            mode: LedMode::Static,
            r1: r,
            g1: g,
            b1: b,
            r2: 0,
            g2: 0,
            b2: 0,
        }
    }

    fn encode_to(&self, buf: &mut [u8]) {
        debug_assert!(buf.len() >= 7);
        buf[0] = self.mode as u8;
        buf[1] = self.r1;
        buf[2] = self.g1;
        buf[3] = self.b1;
        buf[4] = self.r2;
        buf[5] = self.g2;
        buf[6] = self.b2;
    }

    fn decode_from(buf: &[u8]) -> Self {
        debug_assert!(buf.len() >= 7);
        LedSlot {
            mode: match buf[0] {
                0 => LedMode::Off,
                1 => LedMode::Static,
                2 => LedMode::Gradient,
                3 => LedMode::VolumeGradient,
                _ => LedMode::Off,
            },
            r1: buf[1],
            g1: buf[2],
            b1: buf[3],
            r2: buf[4],
            g2: buf[5],
            b2: buf[6],
        }
    }
}

/// Logo LED mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LogoMode {
    Off = 0,
    Static = 1,
    Rainbow = 2,
    Breathing = 3,
}

/// Commands to send to the PCPanel Pro device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidCommand {
    /// Send the init packet to the device.
    Init,
    /// Set LED configuration for all 5 knob rings.
    SetKnobLeds([LedSlot; 5]),
    /// Set LED configuration for all 4 slider labels.
    SetSliderLabelLeds([LedSlot; 4]),
    /// Set LED configuration for all 4 sliders.
    SetSliderLeds([LedSlot; 4]),
    /// Set logo LED.
    SetLogo {
        mode: LogoMode,
        r: u8,
        g: u8,
        b: u8,
        speed: u8,
    },
}

impl HidCommand {
    /// Encode this command into a 64-byte payload (excluding Report ID).
    ///
    /// The caller (HidTransport) is responsible for prepending the Report ID byte.
    #[must_use]
    pub fn encode(&self) -> [u8; REPORT_SIZE] {
        let mut buf = [0u8; REPORT_SIZE];
        match self {
            HidCommand::Init => {
                buf[0] = 0x01;
            }
            HidCommand::SetKnobLeds(slots) => {
                buf[0] = 0x05;
                buf[1] = 0x02;
                for (i, slot) in slots.iter().enumerate() {
                    slot.encode_to(&mut buf[2 + i * 7..]);
                }
            }
            HidCommand::SetSliderLabelLeds(slots) => {
                buf[0] = 0x05;
                buf[1] = 0x01;
                for (i, slot) in slots.iter().enumerate() {
                    slot.encode_to(&mut buf[2 + i * 7..]);
                }
            }
            HidCommand::SetSliderLeds(slots) => {
                buf[0] = 0x05;
                buf[1] = 0x00;
                for (i, slot) in slots.iter().enumerate() {
                    slot.encode_to(&mut buf[2 + i * 7..]);
                }
            }
            HidCommand::SetLogo {
                mode,
                r,
                g,
                b,
                speed,
            } => {
                buf[0] = 0x05;
                buf[1] = 0x03;
                buf[2] = *mode as u8;
                buf[3] = *r;
                buf[4] = *g;
                buf[5] = *b;
                buf[6] = *speed;
            }
        }
        buf
    }

    /// Encode the "all LEDs off" sequence as multiple commands.
    /// Returns the set of commands needed to clear all LEDs.
    #[must_use]
    pub fn all_off_sequence() -> [HidCommand; 4] {
        [
            HidCommand::SetKnobLeds([LedSlot::OFF; 5]),
            HidCommand::SetSliderLabelLeds([LedSlot::OFF; 4]),
            HidCommand::SetSliderLeds([LedSlot::OFF; 4]),
            HidCommand::SetLogo {
                mode: LogoMode::Static,
                r: 0,
                g: 0,
                b: 0,
                speed: 0,
            },
        ]
    }
}

/// Decode a knob LED command payload back into LED slots.
pub fn decode_knob_leds(payload: &[u8; REPORT_SIZE]) -> Option<[LedSlot; 5]> {
    if payload[0] != 0x05 || payload[1] != 0x02 {
        return None;
    }
    let mut slots = [LedSlot::OFF; 5];
    for (i, slot) in slots.iter_mut().enumerate() {
        *slot = LedSlot::decode_from(&payload[2 + i * 7..]);
    }
    Some(slots)
}

/// Decode a slider LED command payload back into LED slots.
pub fn decode_slider_leds(payload: &[u8; REPORT_SIZE]) -> Option<[LedSlot; 4]> {
    if payload[0] != 0x05 || payload[1] != 0x00 {
        return None;
    }
    let mut slots = [LedSlot::OFF; 4];
    for (i, slot) in slots.iter_mut().enumerate() {
        *slot = LedSlot::decode_from(&payload[2 + i * 7..]);
    }
    Some(slots)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_position_event_knob() {
        let report = [0x01, 0x02, 0x80, 0, 0]; // knob 2, value 128
        let event = HidEvent::parse(&report).unwrap();
        assert_eq!(
            event,
            HidEvent::Position {
                control_id: 2,
                value: 128
            }
        );
    }

    #[test]
    fn parse_position_event_slider() {
        let report = [0x01, 0x07, 0xFF]; // slider 2 (analog id 7), value 255
        let event = HidEvent::parse(&report).unwrap();
        assert_eq!(
            event,
            HidEvent::Position {
                control_id: 7,
                value: 255
            }
        );
    }

    #[test]
    fn parse_position_event_endpoints() {
        // Minimum value
        let event = HidEvent::parse(&[0x01, 0x00, 0x00]).unwrap();
        assert_eq!(
            event,
            HidEvent::Position {
                control_id: 0,
                value: 0
            }
        );

        // Maximum value
        let event = HidEvent::parse(&[0x01, 0x08, 0xFF]).unwrap();
        assert_eq!(
            event,
            HidEvent::Position {
                control_id: 8,
                value: 255
            }
        );
    }

    #[test]
    fn parse_position_invalid_control_id() {
        let result = HidEvent::parse(&[0x01, 0x09, 0x80]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_button_press() {
        let event = HidEvent::parse(&[0x02, 0x03, 0x01]).unwrap();
        assert_eq!(
            event,
            HidEvent::Button {
                button_id: 3,
                pressed: true
            }
        );
    }

    #[test]
    fn parse_button_release() {
        let event = HidEvent::parse(&[0x02, 0x03, 0x00]).unwrap();
        assert_eq!(
            event,
            HidEvent::Button {
                button_id: 3,
                pressed: false
            }
        );
    }

    #[test]
    fn parse_button_invalid_id() {
        let result = HidEvent::parse(&[0x02, 0x05, 0x01]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_button_invalid_state() {
        let result = HidEvent::parse(&[0x02, 0x00, 0x02]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_unknown_report_type() {
        let result = HidEvent::parse(&[0x03, 0x00, 0x00]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_too_short() {
        assert!(HidEvent::parse(&[]).is_err());
        assert!(HidEvent::parse(&[0x01]).is_err());
        assert!(HidEvent::parse(&[0x01, 0x00]).is_err());
    }

    #[test]
    fn init_command_encoding() {
        let cmd = HidCommand::Init;
        let buf = cmd.encode();
        assert_eq!(buf[0], 0x01);
        assert!(buf[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn knob_led_encode_decode_round_trip() {
        let slots = [
            LedSlot::static_color(255, 0, 0),
            LedSlot::static_color(0, 255, 0),
            LedSlot::static_color(0, 0, 255),
            LedSlot::static_color(255, 255, 0),
            LedSlot::OFF,
        ];
        let cmd = HidCommand::SetKnobLeds(slots);
        let buf = cmd.encode();

        assert_eq!(buf[0], 0x05);
        assert_eq!(buf[1], 0x02);

        let decoded = decode_knob_leds(&buf).unwrap();
        assert_eq!(decoded, slots);
    }

    #[test]
    fn slider_led_encode_decode_round_trip() {
        let slots = [
            LedSlot::static_color(0, 0, 255),
            LedSlot::static_color(0, 0, 200),
            LedSlot::static_color(0, 0, 150),
            LedSlot::static_color(0, 0, 100),
        ];
        let cmd = HidCommand::SetSliderLeds(slots);
        let buf = cmd.encode();

        assert_eq!(buf[0], 0x05);
        assert_eq!(buf[1], 0x00);

        let decoded = decode_slider_leds(&buf).unwrap();
        assert_eq!(decoded, slots);
    }

    #[test]
    fn logo_command_encoding() {
        let cmd = HidCommand::SetLogo {
            mode: LogoMode::Static,
            r: 255,
            g: 128,
            b: 64,
            speed: 0,
        };
        let buf = cmd.encode();
        assert_eq!(buf[0], 0x05);
        assert_eq!(buf[1], 0x03);
        assert_eq!(buf[2], 1); // Static mode
        assert_eq!(buf[3], 255);
        assert_eq!(buf[4], 128);
        assert_eq!(buf[5], 64);
        assert_eq!(buf[6], 0); // speed
    }

    #[test]
    fn all_off_sequence_clears_everything() {
        let seq = HidCommand::all_off_sequence();
        assert_eq!(seq.len(), 4); // knobs, slider labels, sliders, logo

        for cmd in &seq {
            let buf = cmd.encode();
            // All should be 0x05 commands
            assert_eq!(buf[0], 0x05);
        }
    }

    #[test]
    fn all_off_knobs_are_off() {
        let seq = HidCommand::all_off_sequence();
        let buf = seq[0].encode();
        let slots = decode_knob_leds(&buf).unwrap();
        for slot in &slots {
            assert_eq!(*slot, LedSlot::OFF);
        }
    }

    #[test]
    fn parse_all_valid_position_reports() {
        for control_id in 0..=8u8 {
            for value in [0u8, 1, 127, 128, 254, 255] {
                let event = HidEvent::parse(&[0x01, control_id, value]).unwrap();
                assert_eq!(event, HidEvent::Position { control_id, value });
            }
        }
    }

    #[test]
    fn parse_all_valid_button_reports() {
        for button_id in 0..=4u8 {
            for pressed in [false, true] {
                let event =
                    HidEvent::parse(&[0x02, button_id, if pressed { 1 } else { 0 }]).unwrap();
                assert_eq!(event, HidEvent::Button { button_id, pressed });
            }
        }
    }
}
