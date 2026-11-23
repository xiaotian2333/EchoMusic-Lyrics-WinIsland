use std::cell::RefCell;
use std::time::Instant;

use skia_safe::{
    AlphaType, Color, ColorType, Data, ISize, Image, ImageInfo, Paint, image_filters, images,
    surfaces,
};

use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
};

use crate::utils::gdi_capture::{ScreenCaptureRequest, capture_screen_bgra};

type GlassCacheEntry = (Image, Instant, i32, i32, u32, u32);

thread_local! {
    static GLASS_CACHE: RefCell<Option<GlassCacheEntry>> = const { RefCell::new(None) };
}

/// Frosted dark glass backdrop: captures the island region + margin from the
/// desktop, then applies a heavy blur (sigma ~40). A strong darkening blend
/// (Multiply + dark base) guarantees the signature dark glass look.
///
/// Note: WDA_EXCLUDEFROMCAPTURE is intentionally NOT set. It was previously
/// used to black out the island window during GDI capture, preventing
/// self-feedback. However, it introduced a one-frame lag on window transitions
/// (screenshot tools couldn't capture the island, and every GDI capture had to
/// toggle the affinity flag). The dark Multiply blend layer already masks any
/// residual self-capture artifacts, making WDA unnecessary for glass style.
pub fn get_glass_background(
    screen_x: i32,
    screen_y: i32,
    w: u32,
    h: u32,
    blur_sigma: f32,
) -> Option<Image> {
    if w == 0 || h == 0 {
        return None;
    }

    let cached = GLASS_CACHE.with(|cell| {
        let cache = cell.borrow();
        if let Some((img, time, cx, cy, cw, ch)) = cache.as_ref()
            && time.elapsed().as_millis() < 500
            && *cx == screen_x
            && *cy == screen_y
            && *cw == w
            && *ch == h
        {
            return Some(img.clone());
        }
        None
    });
    if let Some(img) = cached {
        return Some(img);
    }

    let result = capture_and_blur(screen_x, screen_y, w, h, blur_sigma);

    if let Some(ref img) = result {
        GLASS_CACHE.with(|cell| {
            *cell.borrow_mut() = Some((img.clone(), Instant::now(), screen_x, screen_y, w, h));
        });
    }

    result
}

/// 点所在显示器的屏幕边界（左、上、右、下）；查询失败时返回 None。
fn monitor_bounds_at(x: i32, y: i32) -> Option<(i32, i32, i32, i32)> {
    let point = POINT { x, y };
    // SAFETY: point 是栈上有效结构；monitor 与 info 均为本地拥有。
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: monitor 来自 MonitorFromPoint，info 是尺寸正确的输出结构。
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let bounds = info.rcMonitor;
    Some((bounds.left, bounds.top, bounds.right, bounds.bottom))
}

/// Captures the island region + margin from the desktop, heavily blurs,
/// crops to the island area, then blends with a dark base colour to
/// guarantee the signature dark frosted-glass look.
fn capture_and_blur(sx: i32, sy: i32, w: u32, h: u32, blur_sigma: f32) -> Option<Image> {
    let downscale = 4u32;
    // Margin is wide enough that after heavy blur the blacked-out island
    // centre gets diluted by surrounding desktop content, producing a dark
    // tint instead of solid black.
    let margin = (w.max(h) / downscale) as i32;
    let cap_full_w = (w as i32 + 2 * margin).max(1);
    let cap_full_h = (h as i32 + 2 * margin).max(1);

    // 截屏源区域必须裁到所在显示器内：越界时 BitBlt 产出全黑或无意义内容
    let (mon_left, mon_top, mon_right, mon_bottom) = monitor_bounds_at(sx, sy)?;
    let src_left = sx - margin;
    let src_top = sy - margin;
    let clamped_left = src_left.max(mon_left);
    let clamped_top = src_top.max(mon_top);
    let clamped_right = (src_left + cap_full_w).min(mon_right);
    let clamped_bottom = (src_top + cap_full_h).min(mon_bottom);
    if clamped_right <= clamped_left || clamped_bottom <= clamped_top {
        return None;
    }
    // 裁剪偏移对齐到降采样倍数，保证后续按比例还原岛屿区域时无累积误差
    let step = downscale as i32;
    let offset_x = ((clamped_left - src_left) / step) * step;
    let offset_y = ((clamped_top - src_top) / step) * step;
    let src_x = src_left + offset_x;
    let src_y = src_top + offset_y;
    let src_w = clamped_right - src_x;
    let src_h = clamped_bottom - src_y;

    let capture = capture_screen_bgra(ScreenCaptureRequest {
        src_x,
        src_y,
        src_w,
        src_h,
        dst_w: (src_w / step).max(1),
        dst_h: (src_h / step).max(1),
        use_halftone: true,
        force_opaque_alpha: true,
    })?;

    let info = ImageInfo::new(
        ISize::new(capture.width, capture.height),
        ColorType::BGRA8888,
        AlphaType::Opaque,
        None,
    );
    let data = Data::new_copy(&capture.pixels);
    let src_img = images::raster_from_data(&info, data, (capture.width * 4) as usize)?;

    // Frosted glass: heavy blur (sigma ~40, stronger than Mica's ~6).
    let scaled_sigma = blur_sigma / downscale as f32;
    let mut blur_surface = surfaces::raster_n32_premul(ISize::new(capture.width, capture.height))?;
    let blur_canvas = blur_surface.canvas();
    let mut paint = Paint::default();
    if let Some(filter) = image_filters::blur((scaled_sigma, scaled_sigma), None, None, None) {
        paint.set_image_filter(filter);
    }
    blur_canvas.draw_image(&src_img, (0, 0), Some(&paint));
    let blurred = blur_surface.image_snapshot();

    let crop_x = ((margin - offset_x) / downscale as i32).max(0) as f32;
    let crop_y = ((margin - offset_y) / downscale as i32).max(0) as f32;
    let crop_w = (w / downscale).max(1) as i32;
    let crop_h = (h / downscale).max(1) as i32;

    let mut final_surface = surfaces::raster_n32_premul(ISize::new(crop_w, crop_h))?;
    let final_canvas = final_surface.canvas();
    final_canvas.draw_image(&blurred, (-crop_x, -crop_y), None);

    // Blend with a very dark base to guarantee the signature black glass
    // look even when WDA_EXCLUDEFROMCAPTURE doesn't fully black out the
    // island area on the current system.
    let mut darken = Paint::default();
    darken.set_color(Color::from_argb(195, 8, 8, 12));
    darken.set_anti_alias(true);
    darken.set_blend_mode(skia_safe::BlendMode::Multiply);
    final_canvas.draw_paint(&darken);

    Some(final_surface.image_snapshot())
}
