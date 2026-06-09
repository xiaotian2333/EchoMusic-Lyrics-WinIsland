#[cfg(windows)]
#[path = "mouse_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    pub fn get_global_cursor_pos() -> (i32, i32) {
        // TODO: 非 Windows 后续应改为由 winit 鼠标事件维护窗口内坐标，避免依赖平台全局鼠标 API。
        (0, 0)
    }

    pub fn is_left_button_pressed() -> bool {
        false
    }

    pub fn is_cursor_hidden() -> bool {
        false
    }

    pub fn is_foreground_fullscreen() -> bool {
        false
    }
}

pub fn is_point_in_rect(px: f64, py: f64, rx: f64, ry: f64, rw: f64, rh: f64) -> bool {
    px >= rx && px <= rx + rw && py >= ry && py <= ry + rh
}

pub use self::imp::*;
