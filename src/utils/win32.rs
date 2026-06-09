#[cfg(windows)]
#[path = "win32_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    pub fn close_window(_title: &str) {}

    pub fn bring_window_to_front(_title: &str) {}
}

pub use self::imp::*;
