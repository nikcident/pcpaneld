use pcpaneld_core::control::MediaCommand;
use tracing::debug;

/// Send an MPRIS media command to the most appropriate media player on the session bus.
///
/// Finds MPRIS-compliant players via `org.mpris.MediaPlayer2.*` bus names. If multiple
/// players are running, prefers one with `PlaybackStatus == "Playing"`, otherwise picks
/// the first found. If no player is found, logs at debug level and returns Ok (the user
/// simply hasn't started a media player).
pub async fn send_media_command(
    conn: &zbus::Connection,
    cmd: MediaCommand,
) -> Result<(), zbus::Error> {
    let players = list_mpris_players(conn).await?;

    if players.is_empty() {
        debug!("no MPRIS players found, ignoring media command");
        return Ok(());
    }

    // Pick the player that is currently playing, or fall back to the first one.
    let mut target = &players[0];
    for player in &players {
        if is_playing(conn, player).await {
            target = player;
            break;
        }
    }

    debug!("sending MPRIS {} to {target}", cmd.method_name());

    conn.call_method(
        Some(target.as_str()),
        "/org/mpris/MediaPlayer2",
        Some("org.mpris.MediaPlayer2.Player"),
        cmd.method_name(),
        &(),
    )
    .await?;

    Ok(())
}

/// List all MPRIS player bus names on the session bus.
async fn list_mpris_players(conn: &zbus::Connection) -> Result<Vec<String>, zbus::Error> {
    let reply = conn
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "ListNames",
            &(),
        )
        .await?;

    let names: Vec<String> = reply.body().deserialize()?;
    let players: Vec<String> = names
        .into_iter()
        .filter(|n| n.starts_with("org.mpris.MediaPlayer2."))
        .collect();

    Ok(players)
}

/// Check if a player's PlaybackStatus is "Playing".
async fn is_playing(conn: &zbus::Connection, player_name: &str) -> bool {
    try_is_playing(conn, player_name).await.unwrap_or(false)
}

async fn try_is_playing(conn: &zbus::Connection, player_name: &str) -> Option<bool> {
    let reply = conn
        .call_method(
            Some(player_name),
            "/org/mpris/MediaPlayer2",
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.mpris.MediaPlayer2.Player", "PlaybackStatus"),
        )
        .await
        .ok()?;
    // The return type is Variant<String>
    let val: zbus::zvariant::OwnedValue = reply.body().deserialize().ok()?;
    let s: String = val.try_into().ok()?;
    Some(s == "Playing")
}
