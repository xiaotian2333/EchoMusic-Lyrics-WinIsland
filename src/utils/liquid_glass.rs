#[cfg(windows)]
#[path = "liquid_glass_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    use skia_safe::Image;

    pub fn get_liquid_glass_background(
        _screen_x: i32,
        _screen_y: i32,
        _w: u32,
        _h: u32,
        _corner_radius: f32,
        _monitor_x: i32,
        _monitor_y: i32,
        _monitor_w: u32,
        _monitor_h: u32,
    ) -> Option<Image> {
        // TODO: macOS/Linux 后续可接入平台截图或系统材质；当前降级到普通背景。
        None
    }

    pub fn clear_liquid_glass_cache() {}

    pub fn set_exclude_from_capture<T>(_hwnd: T, _exclude: bool) {}
}

#[cfg(windows)]
pub use self::imp::*;

#[cfg(not(windows))]
pub use self::imp::*;
