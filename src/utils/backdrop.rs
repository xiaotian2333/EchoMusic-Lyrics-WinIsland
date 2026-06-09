#[cfg(windows)]
#[path = "backdrop_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    use skia_safe::{Color, Image};

    pub fn disable_mica<T>(_hwnd: T) {}

    pub fn get_mica_background(
        _screen_x: i32,
        _screen_y: i32,
        _w: u32,
        _h: u32,
        _monitor_x: i32,
        _monitor_y: i32,
        _monitor_w: u32,
        _monitor_h: u32,
    ) -> Option<Image> {
        // TODO: macOS/Linux 后续可映射到系统材质或截图模糊；当前不提供 Mica 背景。
        None
    }

    pub fn clear_mica_cache() {}

    pub fn get_dynamic_bg_color(_img: &Image, _cache_key: &str) -> Color {
        // TODO: 可复用 Windows 侧封面取色算法；当前非 Windows 先使用安全暗色。
        Color::from_argb(200, 32, 32, 36)
    }

    pub fn get_last_valid_color() -> Option<Color> {
        None
    }

    pub fn clear_dynamic_bg_cache() {}
}

pub use self::imp::*;
