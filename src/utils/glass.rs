#[cfg(windows)]
#[path = "glass_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    use skia_safe::Image;

    pub fn get_glass_background(
        _screen_x: i32,
        _screen_y: i32,
        _w: u32,
        _h: u32,
        _blur_sigma: f32,
    ) -> Option<Image> {
        // TODO: macOS/Linux 后续可接入平台截图或系统材质；当前降级到普通背景。
        None
    }
}

pub use self::imp::*;
