use std::os::fd::AsRawFd;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use pcpaneld_core::hid::{HidCommand, HidEvent, PRODUCT_ID, VENDOR_ID};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::hid::{HidApiTransport, HidTransport};

/// Button event sent from HID thread to engine.
#[derive(Debug, Clone, Copy)]
pub struct ButtonEvent {
    pub button_id: u8,
    pub pressed: bool,
}

/// Device hotplug events from udev monitor.
#[derive(Debug, Clone, Copy)]
pub enum DeviceEvent {
    Added,
    Removed,
}

/// Main HID thread loop.
///
/// Manages device lifecycle: open -> init -> read loop -> reconnect on disconnect.
/// Uses udev events for instant reconnection instead of polling.
pub fn run(
    config_serial: Option<String>,
    position_tx: watch::Sender<[u8; 9]>,
    button_tx: mpsc::Sender<ButtonEvent>,
    mut cmd_rx: mpsc::Receiver<HidCommand>,
    device_event_rx: std_mpsc::Receiver<DeviceEvent>,
    device_connected_tx: watch::Sender<bool>,
    cancel: CancellationToken,
) {
    let mut api = match hidapi::HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!("failed to initialize HID API: {e}");
            return;
        }
    };

    let mut positions = [0u8; 9];

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Try to open device
        let transport = match HidApiTransport::open(&api, config_serial.as_deref()) {
            Ok(t) => {
                info!(
                    "HID device connected (serial: {})",
                    t.get_serial().as_deref().unwrap_or("unknown")
                );
                t
            }
            Err(e) => {
                debug!("device not found: {e}");
                wait_for_device(&device_event_rx, &cancel);
                if let Err(e) = api.refresh_devices() {
                    warn!("failed to refresh HID device list: {e}");
                }
                continue;
            }
        };

        // Run the device session
        run_device_session(
            &transport,
            &position_tx,
            &button_tx,
            &mut cmd_rx,
            &mut positions,
            &device_connected_tx,
            &cancel,
        );

        // run_device_session returned = device disconnected or errored
        let _ = device_connected_tx.send(false);
        info!("HID device disconnected");
        positions = [0u8; 9];
    }

    info!("HID thread exiting");
}

/// Wait for a udev device event or timeout for fallback polling.
fn wait_for_device(device_event_rx: &std_mpsc::Receiver<DeviceEvent>, cancel: &CancellationToken) {
    match device_event_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(DeviceEvent::Added) => {
            debug!("udev: device added event");
        }
        Ok(DeviceEvent::Removed) => {
            debug!("udev: device removed event (waiting for add)");
        }
        Err(std_mpsc::RecvTimeoutError::Timeout) => {
            if !cancel.is_cancelled() {
                debug!("no udev event, polling for device");
            }
        }
        Err(std_mpsc::RecvTimeoutError::Disconnected) => {
            debug!("udev channel closed");
        }
    }
}

/// Run a single device session: init, drain, read loop.
fn run_device_session(
    transport: &dyn HidTransport,
    position_tx: &watch::Sender<[u8; 9]>,
    button_tx: &mpsc::Sender<ButtonEvent>,
    cmd_rx: &mut mpsc::Receiver<HidCommand>,
    positions: &mut [u8; 9],
    device_connected_tx: &watch::Sender<bool>,
    cancel: &CancellationToken,
) {
    // Send init command
    let init_payload = HidCommand::Init.encode();
    if let Err(e) = transport.write(&init_payload) {
        error!("failed to send init command: {e}");
        return;
    }

    // Drain stale position reports (timeout-based)
    drain_stale_reports(transport);

    // Signal that the device is connected and ready
    let _ = device_connected_tx.send(true);

    // Main read loop
    let mut buf = [0u8; 64];
    loop {
        if cancel.is_cancelled() {
            send_all_off(transport);
            return;
        }

        // Check for outgoing commands (non-blocking)
        while let Ok(cmd) = cmd_rx.try_recv() {
            let payload = cmd.encode();
            if let Err(e) = transport.write(&payload) {
                warn!("failed to write HID command: {e}");
                return;
            }
        }

        // Read with 100ms timeout
        match transport.read_timeout(&mut buf, 100) {
            Ok(0) => continue,
            Ok(n) => match HidEvent::parse(&buf[..n]) {
                Ok(HidEvent::Position { control_id, value }) => {
                    if (control_id as usize) < positions.len() {
                        positions[control_id as usize] = value;
                        position_tx.send_if_modified(|current| {
                            if *current != *positions {
                                *current = *positions;
                                true
                            } else {
                                false
                            }
                        });
                    }
                }
                Ok(HidEvent::Button { button_id, pressed }) => {
                    let event = ButtonEvent { button_id, pressed };
                    if let Err(e) = button_tx.blocking_send(event) {
                        warn!("failed to send button event: {e}");
                        return;
                    }
                }
                Err(e) => {
                    debug!("ignoring malformed HID report: {e}");
                }
            },
            Err(e) => {
                warn!("HID read error: {e}");
                return;
            }
        }
    }
}

/// Drain stale position reports after init.
///
/// The device sends a burst of position reports in response to the init command.
/// These reflect the current physical positions but would cause false "changed"
/// events if fed into the engine, so we discard them here.
fn drain_stale_reports(transport: &dyn HidTransport) {
    let start = Instant::now();
    let mut buf = [0u8; 64];
    let mut count = 0u32;

    loop {
        if start.elapsed() > Duration::from_millis(500) {
            break;
        }
        match transport.read_timeout(&mut buf, 50) {
            Ok(0) => break,
            Ok(_) => {
                count += 1;
            }
            Err(e) => {
                debug!("error during drain: {e}");
                break;
            }
        }
    }

    if count > 0 {
        debug!("drained {count} stale reports after init");
    }
}

/// Send all-off LED commands (best effort, ignore errors).
fn send_all_off(transport: &dyn HidTransport) {
    for cmd in HidCommand::all_off_sequence() {
        let _ = transport.write(&cmd.encode());
    }
}

/// Run the udev monitor on a std::thread.
///
/// `MonitorSocket` is not Send/Sync, so this must run on a dedicated OS thread.
/// Uses poll() for event-driven waiting.
pub fn run_udev_monitor(event_tx: std_mpsc::SyncSender<DeviceEvent>, cancel: CancellationToken) {
    let socket = match udev::MonitorBuilder::new()
        .and_then(|b| b.match_subsystem("hidraw"))
        .and_then(|b| b.listen())
    {
        Ok(s) => s,
        Err(e) => {
            error!("failed to create udev monitor: {e}");
            return;
        }
    };

    let fd = socket.as_raw_fd();
    info!("udev monitor started");

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Poll with 1s timeout
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };

        let ret = unsafe { libc::poll(&mut pollfd, 1, 1000) };

        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            error!("udev poll error: {err}");
            break;
        }

        if ret == 0 {
            continue; // Timeout
        }

        // Process one event per poll cycle
        if let Some(event) = socket.iter().next() {
            // Walk up two levels in the device tree: hidraw → hid → usb_device,
            // where VID/PID attributes live.
            let is_pcpanel = event
                .parent()
                .and_then(|p| p.parent())
                .map(|usb| {
                    let vid = usb
                        .attribute_value("idVendor")
                        .and_then(|v| v.to_str())
                        .and_then(|v| u16::from_str_radix(v, 16).ok());
                    let pid = usb
                        .attribute_value("idProduct")
                        .and_then(|v| v.to_str())
                        .and_then(|v| u16::from_str_radix(v, 16).ok());
                    vid == Some(VENDOR_ID) && pid == Some(PRODUCT_ID)
                })
                .unwrap_or(false);

            if is_pcpanel {
                let dev_event = match event.event_type() {
                    udev::EventType::Add => Some(DeviceEvent::Added),
                    udev::EventType::Remove => Some(DeviceEvent::Removed),
                    _ => None,
                };
                if let Some(evt) = dev_event {
                    debug!("udev: PCPanel Pro {evt:?}");
                    let _ = event_tx.try_send(evt);
                }
            }
        }
    }

    info!("udev monitor exiting");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::MockHidTransport;

    #[test]
    fn drain_stale_reports_empties_queue() {
        let mock = MockHidTransport::new();
        for i in 0..10 {
            mock.queue_read(vec![0x01, 0x00, i]);
        }
        mock.queue_timeout();

        drain_stale_reports(&mock);
        let mut buf = [0u8; 64];
        assert_eq!(mock.read_timeout(&mut buf, 50).unwrap(), 0);
    }

    #[test]
    fn send_all_off_writes_led_clear() {
        let mock = MockHidTransport::new();
        send_all_off(&mock);

        let writes = mock.get_writes();
        assert_eq!(writes.len(), 4);
    }
}
