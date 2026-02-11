use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// Information about the currently focused window, used to match against
/// PulseAudio sink-inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedWindowInfo {
    /// The desktop file ID (e.g., "org.gnome.Ptyxis" or "firefox").
    pub desktop_file: Option<String>,
    /// The X11 resource name (typically the binary name).
    pub resource_name: Option<String>,
    /// The X11 resource class (e.g., "org.gnome.Ptyxis").
    pub resource_class: Option<String>,
    /// The process ID of the focused window.
    pub pid: Option<u32>,
}

const DBUS_SERVICE: &str = "com.pcpaneld.FocusedWindow";
const DBUS_PATH: &str = "/com/pcpaneld/FocusedWindow";
const DBUS_IFACE: &str = "com.pcpaneld.FocusedWindow";
const KWIN_SCRIPT_NAME: &str = "pcpaneld";

/// D-Bus interface that receives focused window updates from the KWin script.
struct FocusedWindowReceiver {
    tx: watch::Sender<Option<FocusedWindowInfo>>,
}

#[zbus::interface(name = "com.pcpaneld.FocusedWindow")]
impl FocusedWindowReceiver {
    fn update(&self, desktop_file: &str, resource_name: &str, resource_class: &str, pid: i32) {
        let non_empty = |s: &str| -> Option<String> {
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        };

        let df = non_empty(desktop_file);
        let rn = non_empty(resource_name);
        let rc = non_empty(resource_class);
        let pid = u32::try_from(pid).ok().filter(|&p| p > 0);

        let info = if df.is_none() && rn.is_none() && rc.is_none() && pid.is_none() {
            None
        } else {
            Some(FocusedWindowInfo {
                desktop_file: df,
                resource_name: rn,
                resource_class: rc,
                pid,
            })
        };

        debug!("KWin script reported focused window: {info:?}");
        let _ = self.tx.send(info);
    }
}

/// Generates the KWin script content. Uses `workspace.windowActivated` signal
/// to push active window info to the daemon via D-Bus, replacing the broken
/// `queryWindowInfo` approach (which triggers the interactive window picker).
fn kwin_script_content() -> String {
    format!(
        r#"function sendWindowInfo(window) {{
    // Ignore null windows (desktop focus, panel focus, etc.). Keeping
    // the last real focused window is correct — a stale window with no
    // audio streams is harmless (the engine finds no matching sink-inputs).
    if (window) {{
        callDBus(
            "{DBUS_SERVICE}", "{DBUS_PATH}", "{DBUS_IFACE}", "Update",
            window.desktopFileName || "",
            window.resourceName || "",
            window.resourceClass || "",
            window.pid || 0
        );
    }}
}}
workspace.windowActivated.connect(sendWindowInfo);
sendWindowInfo(workspace.activeWindow);
"#
    )
}

/// Tracks the currently focused KDE Plasma window by loading a KWin script
/// that pushes updates via D-Bus.
///
/// 1. Registers a D-Bus service (`com.pcpanel.FocusedWindow`) on the session bus
/// 2. Writes a KWin script to `$XDG_RUNTIME_DIR/pcpanel-kwin.js`
/// 3. Loads the script via `org.kde.kwin.Scripting`
/// 4. The script calls back via D-Bus on every window focus change
///
/// Graceful degradation: if any step fails, logs a warning and awaits
/// cancellation. `FocusedApp` targets will silently do nothing.
pub async fn run(focused_tx: watch::Sender<Option<FocusedWindowInfo>>, cancel: CancellationToken) {
    // Step 1: Register D-Bus service BEFORE loading the KWin script.
    // The script fires windowActivated immediately on load, so the service
    // must be ready to receive calls.
    let conn = match setup_dbus_service(focused_tx).await {
        Ok(c) => c,
        Err(e) => {
            warn!("failed to register D-Bus service for focused window tracking: {e}");
            cancel.cancelled().await;
            return;
        }
    };

    // Step 2: Write the KWin script to a temp file.
    let script_path = match write_kwin_script() {
        Ok(p) => p,
        Err(e) => {
            warn!("failed to write KWin script (focused window tracking disabled): {e}");
            cancel.cancelled().await;
            return;
        }
    };

    // Step 3: Load and start the script (cleans up stale registrations first).
    // KWin may not be ready yet at boot — retry with linear backoff, matching
    // the tray retry pattern in tray.rs.
    let mut loaded = false;
    for attempt in 1..=5u64 {
        match load_kwin_script(&conn, &script_path).await {
            Ok(()) => {
                loaded = true;
                break;
            }
            Err(e) => {
                if attempt < 5 {
                    warn!("KWin script load failed (attempt {attempt}/5): {e}");
                    tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
                } else {
                    warn!("KWin script load failed after {attempt} attempts (focused window tracking disabled): {e}");
                }
            }
        }
    }
    if !loaded {
        let _ = std::fs::remove_file(&script_path);
        cancel.cancelled().await;
        return;
    }

    info!("KWin focused window tracking active");

    // Keep the D-Bus service alive until cancellation.
    cancel.cancelled().await;

    // Clean up: unload the KWin script and remove the temp file.
    if let Err(e) = unload_kwin_script(&conn).await {
        debug!("failed to unload KWin script on shutdown: {e}");
    }
    let _ = std::fs::remove_file(&script_path);
}

async fn setup_dbus_service(
    focused_tx: watch::Sender<Option<FocusedWindowInfo>>,
) -> Result<zbus::Connection, zbus::Error> {
    let receiver = FocusedWindowReceiver { tx: focused_tx };

    let conn = zbus::connection::Builder::session()?
        .name(DBUS_SERVICE)?
        .serve_at(DBUS_PATH, receiver)?
        .build()
        .await?;

    Ok(conn)
}

fn script_path() -> PathBuf {
    pcpaneld_core::ipc::xdg_runtime_dir().join("pcpaneld-kwin.js")
}

fn write_kwin_script() -> Result<PathBuf, std::io::Error> {
    let path = script_path();
    std::fs::write(&path, kwin_script_content())?;
    Ok(path)
}

async fn load_kwin_script(
    conn: &zbus::Connection,
    script_path: &std::path::Path,
) -> Result<(), zbus::Error> {
    // Unload any stale script from a previous daemon run (e.g., SIGKILL).
    let _ = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "unloadScript",
            &(KWIN_SCRIPT_NAME,),
        )
        .await;

    let path_str = script_path.to_str().unwrap_or_default();
    let reply = conn
        .call_method(
            Some("org.kde.KWin"),
            "/Scripting",
            Some("org.kde.kwin.Scripting"),
            "loadScript",
            &(path_str, KWIN_SCRIPT_NAME),
        )
        .await?;

    let script_id: i32 = reply.body().deserialize()?;
    debug!("loaded KWin script '{KWIN_SCRIPT_NAME}' with id {script_id}");

    conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "start",
        &(),
    )
    .await?;

    debug!("started KWin scripting engine");
    Ok(())
}

async fn unload_kwin_script(conn: &zbus::Connection) -> Result<(), zbus::Error> {
    conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "unloadScript",
        &(KWIN_SCRIPT_NAME,),
    )
    .await?;
    debug!("unloaded KWin script '{KWIN_SCRIPT_NAME}'");
    Ok(())
}
