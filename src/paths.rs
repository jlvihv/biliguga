//! Where the app keeps its files on disk.
//!
//! One answer for the session and for the player's settings. They used to compute the base
//! directory separately, and the two disagreed on macOS: the session went to
//! `~/Library/Application Support`, the settings to `~/.config`, so "reset the app" meant
//! knowing which file was written by which function.

use std::{env, path::PathBuf};

/// The platform's configuration directory, or `.` when the environment does not say.
pub(crate) fn config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = env::var_os("APPDATA").map(PathBuf::from);

    #[cfg(target_os = "macos")]
    let base = env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library").join("Application Support"));

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base.unwrap_or_else(|| PathBuf::from("."))
}

/// The app's own directory under [`config_dir`].
pub(crate) fn app_dir() -> PathBuf {
    config_dir().join("biliguga")
}
