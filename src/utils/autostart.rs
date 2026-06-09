#[cfg(windows)]
#[path = "autostart_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    pub fn set_autostart(_enabled: bool) -> Result<(), Box<dyn std::error::Error>> {
        // TODO: 后续通过 LaunchAgent（macOS）或 XDG autostart（Linux）实现跨平台开机自启动。
        Ok(())
    }
}

pub use self::imp::*;
