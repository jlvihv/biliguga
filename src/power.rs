use std::process::{Child, Command, Stdio};

/// Keeps the operating system awake while media is actively playing.
///
/// The inhibitor is scoped to the player and is released when playback pauses,
/// ends, or the application exits.
pub struct PowerInhibitor {
    active: bool,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    child: Option<Child>,
}

impl Default for PowerInhibitor {
    fn default() -> Self {
        Self {
            active: false,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            child: None,
        }
    }
}

impl PowerInhibitor {
    pub fn set_active(&mut self, active: bool) {
        if self.active == active {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            if active && self.child.is_some() {
                self.ensure_child();
            }
            return;
        }

        self.active = active;
        if active {
            self.start();
        } else {
            self.stop();
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&mut self) {
        self.stop();
        self.child = Command::new("systemd-inhibit")
            .args([
                "--what=idle:sleep:handle-lid-switch",
                "--who=哔哩咕嘎",
                "--why=视频正在播放",
                "--mode=block",
                "sleep",
                "infinity",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }

    #[cfg(target_os = "macos")]
    fn start(&mut self) {
        self.stop();
        self.child = Command::new("caffeinate")
            .args(["-dims"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok();
    }

    #[cfg(target_os = "windows")]
    fn start(&mut self) {
        set_windows_execution_state(true);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn start(&mut self) {}

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn ensure_child(&mut self) {
        let exited = self
            .child
            .as_mut()
            .map(|child| child.try_wait().map(|status| status.is_some()).unwrap_or(true))
            .unwrap_or(true);
        if exited {
            self.start();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    #[cfg(target_os = "windows")]
    fn stop(&mut self) {
        set_windows_execution_state(false);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    fn stop(&mut self) {}
}

impl Drop for PowerInhibitor {
    fn drop(&mut self) {
        self.active = false;
        self.stop();
    }
}

#[cfg(target_os = "windows")]
const ES_CONTINUOUS: u32 = 0x8000_0000;
#[cfg(target_os = "windows")]
const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
#[cfg(target_os = "windows")]
const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;

#[cfg(target_os = "windows")]
unsafe extern "system" {
    fn SetThreadExecutionState(execution_state: u32) -> u32;
}

#[cfg(target_os = "windows")]
fn set_windows_execution_state(active: bool) {
    let state = if active {
        ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    unsafe {
        let _ = SetThreadExecutionState(state);
    }
}
