use pcpaneld_core::hid::{HidError, PRODUCT_ID, REPORT_SIZE, VENDOR_ID};

/// Abstraction over HID device I/O for testability.
///
/// The real implementation wraps `hidapi::HidDevice`.
/// The mock implementation replays scripted byte sequences.
pub trait HidTransport: Send + 'static {
    /// Read a report with timeout. Returns the number of bytes read (0 = timeout).
    /// The report does NOT include the Report ID byte (kernel strips it on reads).
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, HidError>;

    /// Write a payload to the device. The implementation prepends the Report ID byte.
    /// `data` should be exactly `REPORT_SIZE` (64) bytes.
    fn write(&self, data: &[u8]) -> Result<usize, HidError>;

    /// Get the device serial number, if available.
    fn get_serial(&self) -> Option<String>;
}

/// Real HID transport using hidapi.
pub struct HidApiTransport {
    device: hidapi::HidDevice,
    serial: Option<String>,
}

impl HidApiTransport {
    /// Open a PCPanel Pro device. If `serial` is Some, open that specific device.
    pub fn open(api: &hidapi::HidApi, serial: Option<&str>) -> Result<Self, HidError> {
        let device = match serial {
            Some(s) => api.open_serial(VENDOR_ID, PRODUCT_ID, s),
            None => api.open(VENDOR_ID, PRODUCT_ID),
        }
        .map_err(|e| HidError::Io(e.to_string()))?;

        // Set non-blocking mode off (we use read_timeout for controlled blocking)
        device
            .set_blocking_mode(true)
            .map_err(|e| HidError::Io(e.to_string()))?;

        let serial = device.get_serial_number_string().ok().flatten();

        Ok(HidApiTransport { device, serial })
    }
}

impl HidTransport for HidApiTransport {
    fn read_timeout(&self, buf: &mut [u8], timeout_ms: i32) -> Result<usize, HidError> {
        self.device
            .read_timeout(buf, timeout_ms)
            .map_err(|e| HidError::Io(e.to_string()))
    }

    fn write(&self, data: &[u8]) -> Result<usize, HidError> {
        if data.len() != REPORT_SIZE {
            return Err(HidError::InvalidReport(format!(
                "write payload must be exactly {REPORT_SIZE} bytes, got {}",
                data.len()
            )));
        }
        // Prepend Report ID 0x00 for hidapi write
        let mut buf = [0u8; REPORT_SIZE + 1];
        buf[0] = 0x00; // Report ID
        buf[1..].copy_from_slice(data);

        let written = self
            .device
            .write(&buf)
            .map_err(|e| HidError::Io(e.to_string()))?;

        if written != buf.len() {
            return Err(HidError::ShortWrite {
                expected: buf.len(),
                actual: written,
            });
        }
        Ok(written)
    }

    fn get_serial(&self) -> Option<String> {
        self.serial.clone()
    }
}

/// Mock HID transport for testing. Replays scripted read responses.
#[cfg(test)]
pub struct MockHidTransport {
    reads: std::sync::Mutex<std::collections::VecDeque<Result<Vec<u8>, HidError>>>,
    writes: std::sync::Mutex<Vec<Vec<u8>>>,
    serial: Option<String>,
}

#[cfg(test)]
impl MockHidTransport {
    pub fn new() -> Self {
        MockHidTransport {
            reads: std::sync::Mutex::new(std::collections::VecDeque::new()),
            writes: std::sync::Mutex::new(Vec::new()),
            serial: None,
        }
    }

    pub fn with_serial(mut self, serial: &str) -> Self {
        self.serial = Some(serial.to_string());
        self
    }

    /// Queue a successful read response.
    pub fn queue_read(&self, data: Vec<u8>) {
        self.reads.lock().unwrap().push_back(Ok(data));
    }

    /// Queue a timeout response (0 bytes read).
    pub fn queue_timeout(&self) {
        self.reads.lock().unwrap().push_back(Ok(vec![]));
    }

    /// Queue a read error.
    pub fn queue_read_error(&self, msg: &str) {
        self.reads
            .lock()
            .unwrap()
            .push_back(Err(HidError::Io(msg.to_string())));
    }

    /// Get all writes that were sent to the device.
    pub fn get_writes(&self) -> Vec<Vec<u8>> {
        self.writes.lock().unwrap().clone()
    }
}

#[cfg(test)]
impl HidTransport for MockHidTransport {
    fn read_timeout(&self, buf: &mut [u8], _timeout_ms: i32) -> Result<usize, HidError> {
        let mut reads = self.reads.lock().unwrap();
        match reads.pop_front() {
            Some(Ok(data)) => {
                if data.is_empty() {
                    return Ok(0); // timeout
                }
                let len = data.len().min(buf.len());
                buf[..len].copy_from_slice(&data[..len]);
                Ok(len)
            }
            Some(Err(e)) => Err(e),
            None => Ok(0), // no more scripted data -> timeout
        }
    }

    fn write(&self, data: &[u8]) -> Result<usize, HidError> {
        self.writes.lock().unwrap().push(data.to_vec());
        Ok(data.len() + 1) // +1 for the Report ID that real transport prepends
    }

    fn get_serial(&self) -> Option<String> {
        self.serial.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcpaneld_core::hid::{HidCommand, HidEvent};

    #[test]
    fn mock_transport_read_scripted_events() {
        let mock = MockHidTransport::new();
        // Script a knob turn and a button press
        mock.queue_read(vec![0x01, 0x02, 0x80]);
        mock.queue_read(vec![0x02, 0x00, 0x01]);
        mock.queue_timeout();

        let mut buf = [0u8; 64];

        let n = mock.read_timeout(&mut buf, 100).unwrap();
        assert_eq!(n, 3);
        let event = HidEvent::parse(&buf[..n]).unwrap();
        assert_eq!(
            event,
            HidEvent::Position {
                control_id: 2,
                value: 128
            }
        );

        let n = mock.read_timeout(&mut buf, 100).unwrap();
        assert_eq!(n, 3);
        let event = HidEvent::parse(&buf[..n]).unwrap();
        assert_eq!(
            event,
            HidEvent::Button {
                button_id: 0,
                pressed: true
            }
        );

        // Timeout
        let n = mock.read_timeout(&mut buf, 100).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn mock_transport_captures_writes() {
        let mock = MockHidTransport::new();
        let cmd = HidCommand::Init;
        let payload = cmd.encode();
        mock.write(&payload).unwrap();

        let writes = mock.get_writes();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0][0], 0x01); // init command
    }

    #[test]
    fn mock_transport_serial() {
        let mock = MockHidTransport::new().with_serial("ABC123");
        assert_eq!(mock.get_serial(), Some("ABC123".to_string()));

        let mock2 = MockHidTransport::new();
        assert_eq!(mock2.get_serial(), None);
    }

    #[test]
    fn mock_transport_read_error() {
        let mock = MockHidTransport::new();
        mock.queue_read_error("device disconnected");

        let mut buf = [0u8; 64];
        let result = mock.read_timeout(&mut buf, 100);
        assert!(result.is_err());
    }

    #[test]
    fn mock_exhausted_reads_return_timeout() {
        let mock = MockHidTransport::new();
        // No scripted reads -> should return timeout (0)
        let mut buf = [0u8; 64];
        let n = mock.read_timeout(&mut buf, 100).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn write_all_led_commands_via_mock() {
        use pcpaneld_core::hid::LedSlot;

        let mock = MockHidTransport::new();

        let knob_leds = HidCommand::SetKnobLeds([LedSlot::static_color(255, 255, 255); 5]);
        let slider_leds = HidCommand::SetSliderLeds([LedSlot::static_color(0, 0, 255); 4]);

        mock.write(&knob_leds.encode()).unwrap();
        mock.write(&slider_leds.encode()).unwrap();

        let writes = mock.get_writes();
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0][0], 0x05);
        assert_eq!(writes[0][1], 0x02); // knobs
        assert_eq!(writes[1][0], 0x05);
        assert_eq!(writes[1][1], 0x00); // sliders
    }
}
