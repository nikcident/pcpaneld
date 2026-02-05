use std::cell::RefCell;
use std::rc::Rc;

use libpulse_binding as pulse;
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::subscribe::{Facility, InterestMaskSet};
use libpulse_binding::context::{Context, FlagSet as CtxFlagSet, State as CtxState};
use libpulse_binding::mainloop::threaded::Mainloop;
use libpulse_binding::proplist::Proplist;
use pcpaneld_core::audio::{AudioState, SinkInfo, SinkInputInfo, SourceInfo, Volume};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Commands from the engine to the PA thread.
#[derive(Debug)]
pub enum AudioCommand {
    SinkVolume {
        index: u32,
        volume: Volume,
        channels: u8,
    },
    SinkMute {
        index: u32,
        mute: bool,
    },
    SourceVolume {
        index: u32,
        volume: Volume,
        channels: u8,
    },
    SourceMute {
        index: u32,
        mute: bool,
    },
    SinkInputVolume {
        index: u32,
        volume: Volume,
        channels: u8,
    },
    SinkInputMute {
        index: u32,
        mute: bool,
    },
}

/// Notifications from the PA thread to the engine.
#[derive(Debug)]
pub enum AudioNotification {
    Connected,
    Disconnected,
    StateSnapshot(AudioState),
}

/// Main PulseAudio thread function.
///
/// Runs the PA threaded mainloop, subscribes to events, and communicates
/// with the engine via channels.
pub fn run(
    mut cmd_rx: mpsc::Receiver<AudioCommand>,
    notify_tx: mpsc::Sender<AudioNotification>,
    cancel: CancellationToken,
) {
    let mut retry_delay_ms: u64 = 1000;
    loop {
        if cancel.is_cancelled() {
            break;
        }

        let session_start = std::time::Instant::now();
        match run_session(&mut cmd_rx, &notify_tx, &cancel) {
            Ok(()) => {
                break;
            }
            Err(e) => {
                warn!("PulseAudio session ended: {e}");
                let _ = notify_tx.blocking_send(AudioNotification::Disconnected);
                if cancel.is_cancelled() {
                    return;
                }
                // Reset backoff if session was stable (ran >30s)
                if session_start.elapsed() > std::time::Duration::from_secs(30) {
                    retry_delay_ms = 1000;
                }
                std::thread::sleep(std::time::Duration::from_millis(retry_delay_ms));
                retry_delay_ms = (retry_delay_ms * 2).min(4000);
            }
        }
    }

    info!("PulseAudio thread exiting");
}

fn run_session(
    cmd_rx: &mut mpsc::Receiver<AudioCommand>,
    notify_tx: &mpsc::Sender<AudioNotification>,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    // Safety: Rc<RefCell> is used here because this entire function runs on a single
    // std::thread. The PA mainloop is synchronous — all subscribe callbacks execute
    // on this thread during mainloop.iterate(). No cross-thread sharing occurs.
    let mainloop =
        Rc::new(RefCell::new(Mainloop::new().ok_or_else(|| {
            anyhow::anyhow!("failed to create PA mainloop")
        })?));

    let mut proplist =
        Proplist::new().ok_or_else(|| anyhow::anyhow!("failed to create PA proplist"))?;
    proplist
        .set_str(pulse::proplist::properties::APPLICATION_NAME, "PCPanel Pro")
        .map_err(|_| anyhow::anyhow!("failed to set proplist"))?;

    let context = Rc::new(RefCell::new(
        Context::new_with_proplist(&*mainloop.borrow(), "PCPanel Pro", &proplist)
            .ok_or_else(|| anyhow::anyhow!("failed to create PA context"))?,
    ));

    // Connect (before mainloop starts, no lock needed)
    context
        .borrow_mut()
        .connect(None, CtxFlagSet::NOFLAGS, None)
        .map_err(|e| anyhow::anyhow!("PA connect failed: {e}"))?;

    mainloop
        .borrow_mut()
        .start()
        .map_err(|e| anyhow::anyhow!("PA mainloop start failed: {e}"))?;

    // Wait for context to be ready.
    // Must hold the mainloop lock when accessing the context from our thread.
    loop {
        if cancel.is_cancelled() {
            mainloop.borrow_mut().stop();
            return Ok(());
        }

        mainloop.borrow_mut().lock();
        let state = context.borrow().get_state();
        mainloop.borrow_mut().unlock();

        match state {
            CtxState::Ready => break,
            CtxState::Failed | CtxState::Terminated => {
                mainloop.borrow_mut().stop();
                return Err(anyhow::anyhow!("PA context failed to connect"));
            }
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    info!("PulseAudio connected");
    let _ = notify_tx.blocking_send(AudioNotification::Connected);

    // Set up subscribe callback for change notifications.
    // The dirty flag is accessed from our thread (under PA lock) and from the
    // mainloop thread's subscribe callback (which runs with the lock held).
    let dirty = Rc::new(RefCell::new(true)); // Start dirty to get initial snapshot
    let dirty_for_cb = dirty.clone();

    mainloop.borrow_mut().lock();
    {
        let mut ctx = context.borrow_mut();
        ctx.set_subscribe_callback(Some(Box::new(move |facility, _operation, _index| {
            if let Some(
                Facility::Sink | Facility::Source | Facility::SinkInput | Facility::Server,
            ) = facility
            {
                *dirty_for_cb.borrow_mut() = true;
            }
        })));

        ctx.subscribe(
            InterestMaskSet::SINK
                | InterestMaskSet::SOURCE
                | InterestMaskSet::SINK_INPUT
                | InterestMaskSet::SERVER,
            |_success| {},
        );
    }
    mainloop.borrow_mut().unlock();

    loop {
        if cancel.is_cancelled() {
            break;
        }

        // Lock the mainloop for all PA API calls in this iteration
        mainloop.borrow_mut().lock();

        // Check for context health
        let state = context.borrow().get_state();
        if state != CtxState::Ready {
            mainloop.borrow_mut().unlock();
            mainloop.borrow_mut().stop();
            return Err(anyhow::anyhow!("PA context disconnected"));
        }

        // Process audio commands (non-blocking)
        let mut had_work = false;
        while let Ok(cmd) = cmd_rx.try_recv() {
            execute_command(&context, cmd);
            had_work = true;
        }

        // If dirty, start a new snapshot
        if *dirty.borrow() {
            had_work = true;
            *dirty.borrow_mut() = false;
            let ctx = SnapshotCtx {
                state: Rc::new(RefCell::new(AudioState::default())),
                pending: Rc::new(RefCell::new(4u32)), // 4 queries
                notify: notify_tx.clone(),
                dirty: dirty.clone(),
            };

            // Query server info (for default sink/source names)
            {
                let ctx = ctx.clone();
                context.borrow().introspect().get_server_info(move |info| {
                    ctx.state.borrow_mut().default_sink_name =
                        info.default_sink_name.as_ref().map(|s| s.to_string());
                    ctx.state.borrow_mut().default_source_name =
                        info.default_source_name.as_ref().map(|s| s.to_string());
                    ctx.complete();
                });
            }

            // Query sinks
            {
                let ctx = ctx.clone();
                context
                    .borrow()
                    .introspect()
                    .get_sink_info_list(move |result| {
                        if let ListResult::Item(info) = result {
                            let vol = pulse::volume::VolumeLinear::from(info.volume.avg()).0;
                            ctx.state.borrow_mut().sinks.push(SinkInfo {
                                index: info.index,
                                name: info
                                    .name
                                    .as_ref()
                                    .map(|s| s.to_string())
                                    .unwrap_or_default(),
                                description: info
                                    .description
                                    .as_ref()
                                    .map(|s| s.to_string())
                                    .unwrap_or_default(),
                                volume: Volume::new(vol),
                                muted: info.mute,
                                channels: info.volume.len(),
                            });
                        } else if let ListResult::End = result {
                            ctx.complete();
                        }
                    });
            }

            // Query sources
            {
                let ctx = ctx.clone();
                context
                    .borrow()
                    .introspect()
                    .get_source_info_list(move |result| {
                        if let ListResult::Item(info) = result {
                            let vol = pulse::volume::VolumeLinear::from(info.volume.avg()).0;
                            ctx.state.borrow_mut().sources.push(SourceInfo {
                                index: info.index,
                                name: info
                                    .name
                                    .as_ref()
                                    .map(|s| s.to_string())
                                    .unwrap_or_default(),
                                description: info
                                    .description
                                    .as_ref()
                                    .map(|s| s.to_string())
                                    .unwrap_or_default(),
                                volume: Volume::new(vol),
                                muted: info.mute,
                                channels: info.volume.len(),
                            });
                        } else if let ListResult::End = result {
                            ctx.complete();
                        }
                    });
            }

            // Query sink-inputs
            {
                let ctx = ctx;
                context
                    .borrow()
                    .introspect()
                    .get_sink_input_info_list(move |result| {
                        if let ListResult::Item(info) = result {
                            let vol = pulse::volume::VolumeLinear::from(info.volume.avg()).0;
                            let binary = info.proplist.get_str("application.process.binary");
                            let flatpak_id = info.proplist.get_str("application.flatpak.id");
                            let pid = info
                                .proplist
                                .get_str("application.process.id")
                                .and_then(|s| s.parse::<u32>().ok());
                            let name = info
                                .name
                                .as_ref()
                                .map(|s| s.to_string())
                                .or_else(|| info.proplist.get_str("application.name"))
                                .unwrap_or_default();

                            ctx.state.borrow_mut().sink_inputs.push(SinkInputInfo {
                                index: info.index,
                                name,
                                binary,
                                flatpak_id,
                                pid,
                                sink_index: info.sink,
                                volume: Volume::new(vol),
                                muted: info.mute,
                                channels: info.volume.len(),
                            });
                        } else if let ListResult::End = result {
                            ctx.complete();
                        }
                    });
            }
        }

        mainloop.borrow_mut().unlock();

        // Sleep briefly to avoid busy-spinning (lock is NOT held during sleep).
        // Use a shorter interval when actively processing commands/events.
        let sleep_ms = if had_work { 20 } else { 100 };
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }

    // Clean shutdown
    mainloop.borrow_mut().lock();
    context.borrow_mut().disconnect();
    mainloop.borrow_mut().unlock();
    mainloop.borrow_mut().stop();
    Ok(())
}

/// Bundles the shared state for a single PA snapshot query cycle.
///
/// Each of the 4 introspection queries clones this struct once instead of
/// cloning 4 individual `Rc`s.
#[derive(Clone)]
struct SnapshotCtx {
    state: Rc<RefCell<AudioState>>,
    pending: Rc<RefCell<u32>>,
    notify: mpsc::Sender<AudioNotification>,
    dirty: Rc<RefCell<bool>>,
}

impl SnapshotCtx {
    fn complete(&self) {
        let mut p = self.pending.borrow_mut();
        *p = p.saturating_sub(1);
        if *p == 0 {
            let snapshot = self.state.borrow().clone();
            debug!(
                "PA snapshot: {} sinks, {} sources, {} inputs",
                snapshot.sinks.len(),
                snapshot.sources.len(),
                snapshot.sink_inputs.len(),
            );
            let _ = self
                .notify
                .blocking_send(AudioNotification::StateSnapshot(snapshot));

            // If dirty flag was set during query, we'll re-snapshot on next iteration
            if *self.dirty.borrow() {
                debug!("dirty flag set during snapshot, will re-query");
            }
        }
    }
}

fn make_channel_volumes(volume: Volume, channels: u8) -> pulse::volume::ChannelVolumes {
    let pa_vol = volume_to_pa(volume);
    let mut cv = pulse::volume::ChannelVolumes::default();
    cv.set(channels, pa_vol);
    cv
}

/// Execute a PA command. Caller must hold the mainloop lock.
fn execute_command(context: &Rc<RefCell<Context>>, cmd: AudioCommand) {
    let mut introspect = context.borrow().introspect();

    match cmd {
        AudioCommand::SinkVolume {
            index,
            volume,
            channels,
        } => {
            let cv = make_channel_volumes(volume, channels);
            introspect.set_sink_volume_by_index(index, &cv, None);
        }
        AudioCommand::SinkMute { index, mute } => {
            introspect.set_sink_mute_by_index(index, mute, None);
        }
        AudioCommand::SourceVolume {
            index,
            volume,
            channels,
        } => {
            let cv = make_channel_volumes(volume, channels);
            introspect.set_source_volume_by_index(index, &cv, None);
        }
        AudioCommand::SourceMute { index, mute } => {
            introspect.set_source_mute_by_index(index, mute, None);
        }
        AudioCommand::SinkInputVolume {
            index,
            volume,
            channels,
        } => {
            let cv = make_channel_volumes(volume, channels);
            introspect.set_sink_input_volume(index, &cv, None);
        }
        AudioCommand::SinkInputMute { index, mute } => {
            introspect.set_sink_input_mute(index, mute, None);
        }
    }
}

fn volume_to_pa(volume: Volume) -> pulse::volume::Volume {
    pulse::volume::Volume::from(pulse::volume::VolumeLinear(volume.get()))
}
