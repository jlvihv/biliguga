//! The player's volume and speed, on disk, and the defaults when the file is missing.

pub(crate) use super::*;

/// Where the player's volume and speed live.
pub(crate) fn player_settings_path() -> PathBuf {
    crate::paths::app_dir().join("settings.json")
}

pub(crate) fn load_player_settings() -> (f64, f64) {
    let defaults = (DEFAULT_VOLUME, DEFAULT_SPEED);
    let Ok(content) = fs::read_to_string(player_settings_path()) else {
        return defaults;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return defaults;
    };
    let volume = value
        .get("volume")
        .and_then(serde_json::Value::as_f64)
        .filter(|volume| volume.is_finite())
        .map(|volume| volume.clamp(0., 100.))
        .unwrap_or(DEFAULT_VOLUME);
    let speed = value
        .get("speed")
        .and_then(serde_json::Value::as_f64)
        .filter(|speed| speed.is_finite() && *speed >= 0.1)
        .unwrap_or(DEFAULT_SPEED);
    (volume, speed)
}

pub(crate) fn save_player_settings(volume: f64, speed: f64) {
    let path = player_settings_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let value = serde_json::json!({
        "volume": volume.clamp(0., 100.),
        "speed": speed.max(0.1),
    });
    let _ = fs::write(path, value.to_string());
}
