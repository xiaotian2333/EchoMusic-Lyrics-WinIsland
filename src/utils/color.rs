use std::cell::{Cell, RefCell};

use skia_safe::{
    AlphaType, Color, ColorType, FilterMode, HSV, ISize, Image, ImageInfo, MipmapMode, Paint,
    Point, Rect, SamplingOptions, TileMode, gradient_shader, image, surfaces,
};

/// 饱和度低于该值的颜色视为无彩色，色相没有意义，插值时退回 RGB 线性过渡。
const ACHROMATIC_SATURATION: f32 = 0.05;
/// 色相环上相隔达到该角度即视为互补色一类：线性混合的中点会明显发灰，改走色相环。
const COMPLEMENTARY_HUE_SPAN: f32 = 120.0;
/// 色相环插值的取样点数，取奇数以便有一个精确的中点色。
const HUE_RING_STEPS: usize = 9;

// 封面取色（参考 EchoMusic 的主题强调色取色：extractDominantColor + normalizeAccent）。
/// 采样边长：先缩到该尺寸再逐像素投票，代价与封面原尺寸无关。
const COVER_SAMPLE_SIZE: i32 = 64;
/// 色相分桶数量，每桶 20°。
const COVER_HUE_BUCKETS: usize = 18;
/// 明度低于该值的像素视为近黑，不参与投票。
const COVER_DARK_LIMIT: f32 = 0.15;
/// 明度高于该值的像素视为近白，不参与投票。
const COVER_LIGHT_LIMIT: f32 = 0.92;
/// 饱和度低于该值的像素视为灰，不参与投票。
const COVER_MIN_PIXEL_SATURATION: f32 = 0.18;
/// 封面平均饱和度低于该值时视为无彩色封面，不再取色。
const COVER_MIN_MEAN_SATURATION: f32 = 0.15;
/// 中亮度偏好：明度偏离 0.5 时评分的衰减系数。
const COVER_LIGHTNESS_PENALTY: f32 = 1.2;
/// 评分下限，避免极暗或极亮的像素完全不计票。
const COVER_MIN_SCORE: f32 = 0.15;
/// 已播放色规范化后的饱和度区间，保证在深色岛上鲜艳可辨又不过艳。
const PLAYED_SATURATION_RANGE: (f32, f32) = (0.45, 0.85);
/// 已播放色规范化后的明度区间，兼顾深色岛上的可读性与色彩厚度。
const PLAYED_LIGHTNESS_RANGE: (f32, f32) = (0.55, 0.72);
/// 主色饱和度低于该值视为灰，回落兜底色。
const PLAYED_MIN_SATURATION: f32 = 0.08;
/// 封面确实只有黑白灰、或取色失败时的兜底浅蓝。
pub const PLAYED_FALLBACK_COLOR: Color = Color::from_rgb(120, 190, 255);

/// 液态玻璃未播放歌词的浅色档：背景偏暗时使用。
const LIQUID_UNPLAYED_LIGHT: Color = Color::WHITE;
/// 液态玻璃未播放歌词的深色档：背景偏亮时使用；纯黑落在玻璃上过于生硬。
const LIQUID_UNPLAYED_DARK: Color = Color::from_rgb(20, 20, 20);
/// 背景显示亮度低于该值即切到浅色档。
const LIQUID_UNPLAYED_LIGHT_BELOW: f32 = 0.45;
/// 背景显示亮度高于该值即切到深色档；两阈值之间是滞回区。
const LIQUID_UNPLAYED_DARK_ABOVE: f32 = 0.55;
/// 换档时灰度的过渡速度，按 dt/60 归一化；4.0 约等于 0.25 秒走完全程。
const LIQUID_UNPLAYED_FADE_SPEED: f32 = 4.0;

thread_local! {
    /// 未播放歌词的当前档位（true = 亮背景档，深色文字）与显示中的灰度值。
    static LIQUID_UNPLAYED: Cell<(bool, f32)> = const { Cell::new((false, 255.0)) };
    /// 已播放色的自动取值缓存：同一张封面只统计一次。
    static PLAYED_COLOR_CACHE: RefCell<Option<(String, Color)>> = const { RefCell::new(None) };
}

pub fn color_with_alpha(color: Color, alpha: u8) -> Color {
    Color::from_argb(alpha, color.r(), color.g(), color.b())
}

/// 逐字歌词已播放色的自动取值：从封面图像里按色相投票选出主色。
///
/// 算法参考 EchoMusic 的主题强调色取色（`extractDominantColor` + `normalizeAccent`）：
/// 封面先缩到 64×64，逐像素剔除近黑、近白与灰像素，再按「饱和度 × 中亮度偏好」给
/// 18 个色相桶加权投票；票数最高的色相桶取加权平均色，最后把 HSL 的饱和度与明度
/// 钳到深色背景上可读的区间。封面整体偏灰、或取色失败时返回浅蓝兜底色。
///
/// `cache_key` 用于缓存同一张封面的结果：绘制每帧都会取一次色。
pub fn pick_played_color(img: &Image, cache_key: &str) -> Color {
    PLAYED_COLOR_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((key, color)) = cache.as_ref()
            && key == cache_key
        {
            return *color;
        }
        let color = cover_dominant_color(img)
            .map(normalize_played_color)
            .unwrap_or(PLAYED_FALLBACK_COLOR);
        *cache = Some((cache_key.to_string(), color));
        color
    })
}

/// 缩到固定尺寸后逐像素投票，返回封面主色；封面偏灰或读取失败时返回 None。
fn cover_dominant_color(img: &Image) -> Option<Color> {
    let size = ISize::new(COVER_SAMPLE_SIZE, COVER_SAMPLE_SIZE);
    let mut surface = surfaces::raster_n32_premul(size)?;
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    // 缩放到固定尺寸：代价与封面原尺寸无关，且与 EchoMusic 的 64×64 采样口径一致
    surface.canvas().draw_image_rect_with_sampling_options(
        img,
        None,
        Rect::from_xywh(0.0, 0.0, COVER_SAMPLE_SIZE as f32, COVER_SAMPLE_SIZE as f32),
        SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear),
        &paint,
    );
    let scaled = surface.image_snapshot();
    let info = ImageInfo::new(size, ColorType::BGRA8888, AlphaType::Premul, None);
    let mut pixels = vec![0_u8; (COVER_SAMPLE_SIZE * COVER_SAMPLE_SIZE * 4) as usize];
    if !scaled.read_pixels(
        &info,
        &mut pixels,
        (COVER_SAMPLE_SIZE * 4) as usize,
        (0, 0),
        image::CachingHint::Allow,
    ) {
        return None;
    }
    dominant_color_from_bgra(&pixels)
}

/// 按色相分桶投票选出主色；输入是 BGRA 像素缓冲，整体偏灰时返回 None。
fn dominant_color_from_bgra(pixels: &[u8]) -> Option<Color> {
    // 每桶累加 (权重, r, g, b)
    let mut buckets = [(0.0_f32, 0.0_f32, 0.0_f32, 0.0_f32); COVER_HUE_BUCKETS];
    let mut sampled = 0_u32;
    let mut saturation_sum = 0.0_f32;
    for pixel in pixels.chunks_exact(4) {
        if pixel[3] < 200 {
            continue;
        }
        let (r, g, b) = (
            f32::from(pixel[2]),
            f32::from(pixel[1]),
            f32::from(pixel[0]),
        );
        let (hue, saturation, lightness) = rgb_to_hsl(r, g, b);
        // 近黑、近白与灰像素不参与投票
        if !(COVER_DARK_LIMIT..=COVER_LIGHT_LIMIT).contains(&lightness)
            || saturation < COVER_MIN_PIXEL_SATURATION
        {
            continue;
        }
        sampled += 1;
        saturation_sum += saturation;
        // 越饱和、明度越接近中值的像素票数越高
        let lightness_score = 1.0 - (lightness - 0.5).abs() * COVER_LIGHTNESS_PENALTY;
        let score = saturation * lightness_score.max(COVER_MIN_SCORE);
        let index =
            ((hue / (360.0 / COVER_HUE_BUCKETS as f32)) as usize).min(COVER_HUE_BUCKETS - 1);
        let bucket = &mut buckets[index];
        bucket.0 += score;
        bucket.1 += r * score;
        bucket.2 += g * score;
        bucket.3 += b * score;
    }
    // 没采到像素，或封面整体太灰，放弃取色
    if sampled == 0 || saturation_sum / (sampled as f32) < COVER_MIN_MEAN_SATURATION {
        return None;
    }
    let best = buckets.iter().max_by(|a, b| a.0.total_cmp(&b.0))?;
    if best.0 <= 0.0 {
        return None;
    }
    Some(Color::from_rgb(
        (best.1 / best.0).round().clamp(0.0, 255.0) as u8,
        (best.2 / best.0).round().clamp(0.0, 255.0) as u8,
        (best.3 / best.0).round().clamp(0.0, 255.0) as u8,
    ))
}

/// 把主色钳到深色背景上可读的饱和度与明度区间，色相保持不变。
fn normalize_played_color(color: Color) -> Color {
    let (hue, saturation, lightness) = rgb_to_hsl(
        f32::from(color.r()),
        f32::from(color.g()),
        f32::from(color.b()),
    );
    if saturation < PLAYED_MIN_SATURATION {
        return PLAYED_FALLBACK_COLOR;
    }
    let (min_saturation, max_saturation) = PLAYED_SATURATION_RANGE;
    let (min_lightness, max_lightness) = PLAYED_LIGHTNESS_RANGE;
    hsl_to_color(
        hue,
        saturation.clamp(min_saturation, max_saturation),
        lightness.clamp(min_lightness, max_lightness),
    )
}

/// RGB（0~255）转 HSL：色相 0~360，饱和度与明度 0~1。
///
/// 取色算法用的是 HSL 明度 `(max+min)/2`，与 Skia 的 HSV 明度不是一回事，
/// 因此这里单独换算。
fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (r, g, b) = (r / 255.0, g / 255.0, b / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let lightness = (max + min) / 2.0;
    if delta <= f32::EPSILON {
        return (0.0, 0.0, lightness);
    }
    let saturation = (delta / (1.0 - (2.0 * lightness - 1.0).abs())).clamp(0.0, 1.0);
    let hue = if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    } * 60.0;
    (hue, saturation, lightness)
}

/// HSL 转不透明的 Skia 颜色。
fn hsl_to_color(hue: f32, saturation: f32, lightness: f32) -> Color {
    let saturation = saturation.clamp(0.0, 1.0);
    let lightness = lightness.clamp(0.0, 1.0);
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let second = chroma * (1.0 - (sector % 2.0 - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, second, 0.0),
        1 => (second, chroma, 0.0),
        2 => (0.0, chroma, second),
        3 => (0.0, second, chroma),
        4 => (second, 0.0, chroma),
        _ => (chroma, 0.0, second),
    };
    let offset = lightness - chroma / 2.0;
    let channel = |value: f32| ((value + offset) * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::from_rgb(channel(r), channel(g), channel(b))
}

/// 液态玻璃模式下未播放歌词的自动颜色：按背景显示亮度在黑白两档间取值。
///
/// 背景越亮文字越深，两档各自是无彩色，保证任何背景下都清晰。两阈值之间是
/// 滞回区：亮度在其中波动时保持上一档，避免文字在临界亮度上反复跳色；换档
/// 本身也按 `dt` 平滑过渡而非硬切。
///
/// `display_luma` 是玻璃着色**之后**的显示亮度而非桌面内容亮度，换算见
/// [`crate::utils::liquid_glass::display_luminance`]。
pub fn liquid_unplayed_color(display_luma: f32, dt: f32) -> Color {
    LIQUID_UNPLAYED.with(|cell| {
        let (mut on_light, mut gray) = cell.get();
        // 只有越过对侧阈值才换档，两阈值之间维持上一档
        if display_luma >= LIQUID_UNPLAYED_DARK_ABOVE {
            on_light = true;
        } else if display_luma <= LIQUID_UNPLAYED_LIGHT_BELOW {
            on_light = false;
        }
        let target = f32::from(if on_light {
            LIQUID_UNPLAYED_DARK.b()
        } else {
            LIQUID_UNPLAYED_LIGHT.b()
        });
        let step = LIQUID_UNPLAYED_FADE_SPEED * dt / 60.0 * 255.0;
        gray = if (target - gray).abs() <= step {
            target
        } else {
            gray + step.copysign(target - gray)
        };
        cell.set((on_light, gray));
        let value = gray.round().clamp(0.0, 255.0) as u8;
        Color::from_rgb(value, value, value)
    })
}

pub fn parse_hex_color(hex: &str) -> Option<Color> {
    let hex = hex.trim();
    if hex.is_empty() || hex.eq_ignore_ascii_case("auto") {
        return None;
    }
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color::from_rgb(r, g, b))
}

pub fn lyric_boundary_gradient_shader(
    boundary_x: f32,
    y: f32,
    transition_half_width: f32,
    played_color: Color,
    unplayed_color: Color,
    alpha: u8,
) -> Option<skia_safe::Shader> {
    if !boundary_x.is_finite() || !y.is_finite() {
        return None;
    }

    let half_width = transition_half_width.max(0.5);
    let colors = boundary_gradient_colors(played_color, unplayed_color, alpha);
    gradient_shader::linear(
        (
            Point::new(boundary_x - half_width, y),
            Point::new(boundary_x + half_width, y),
        ),
        colors.as_slice(),
        None,
        TileMode::Clamp,
        None,
        None,
    )
}

/// 逐字边界渐变的取色序列。
///
/// 默认沿用 RGB 线性过渡；只有两色属于互补色一类（色相环上相隔很远）时才改沿
/// HSV 色相环走较短弧——此时线性混合的中点会明显发灰，例如蓝与红的中点会掉到
/// 半明度的暗紫。任一色接近无彩色（例如默认的白色未播放色）时色相没有意义，
/// 一律保持线性过渡。
fn boundary_gradient_colors(played: Color, unplayed: Color, alpha: u8) -> Vec<Color> {
    let from = played.to_hsv();
    let to = unplayed.to_hsv();
    // 黑白灰到彩色不触发色相环
    if from.s < ACHROMATIC_SATURATION || to.s < ACHROMATIC_SATURATION {
        return linear_gradient_colors(played, unplayed, alpha);
    }
    // 超过半圈时反向绕行，始终取较短弧
    let mut delta = to.h - from.h;
    if delta > 180.0 {
        delta -= 360.0;
    } else if delta < -180.0 {
        delta += 360.0;
    }
    // 非互补色保持原算法的线性过渡观感
    if delta.abs() < COMPLEMENTARY_HUE_SPAN {
        return linear_gradient_colors(played, unplayed, alpha);
    }
    let last = (HUE_RING_STEPS - 1) as f32;
    (0..HUE_RING_STEPS)
        .map(|index| {
            let t = index as f32 / last;
            HSV {
                h: (from.h + delta * t).rem_euclid(360.0),
                s: from.s + (to.s - from.s) * t,
                v: from.v + (to.v - from.v) * t,
            }
            .to_color(alpha)
        })
        .collect()
}

/// 原算法：两色 RGB 线性过渡，仅套用透明度。
fn linear_gradient_colors(played: Color, unplayed: Color, alpha: u8) -> Vec<Color> {
    vec![
        color_with_alpha(played, alpha),
        color_with_alpha(unplayed, alpha),
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        HUE_RING_STEPS, LIQUID_UNPLAYED, LIQUID_UNPLAYED_DARK, LIQUID_UNPLAYED_LIGHT,
        PLAYED_FALLBACK_COLOR, PLAYED_LIGHTNESS_RANGE, PLAYED_SATURATION_RANGE,
        boundary_gradient_colors, cover_dominant_color, dominant_color_from_bgra, hsl_to_color,
        liquid_unplayed_color, normalize_played_color, pick_played_color, rgb_to_hsl,
    };
    use skia_safe::{Color, ISize, surfaces};

    /// 复位未播放歌词的档位与灰度状态，避免用例之间互相影响。
    fn reset_liquid_unplayed() {
        LIQUID_UNPLAYED.with(|cell| cell.set((false, 255.0)));
    }

    /// 以固定背景亮度推进 1 秒（60 帧），返回最终颜色；过渡此时必然已走完。
    fn liquid_unplayed_after_one_second(display_luma: f32) -> Color {
        (0..60).fold(Color::WHITE, |_, _| {
            liquid_unplayed_color(display_luma, 1.0)
        })
    }

    /// 暗背景取浅色档、亮背景取深色档，且两档都不掺色相。
    #[test]
    fn liquid_unplayed_color_follows_background_luminance() {
        reset_liquid_unplayed();
        let dark_background = liquid_unplayed_after_one_second(0.1);
        assert_eq!(dark_background, LIQUID_UNPLAYED_LIGHT);

        reset_liquid_unplayed();
        let light_background = liquid_unplayed_after_one_second(0.9);
        assert_eq!(light_background, LIQUID_UNPLAYED_DARK);

        for color in [dark_background, light_background] {
            let hsv = color.to_hsv();
            assert!(hsv.s < 0.05, "未播放歌词色必须落在黑白灰里: {hsv:?}");
        }
    }

    /// 亮度落在滞回区内时保持上一档，避免文字在临界亮度上反复跳色。
    #[test]
    fn liquid_unplayed_color_holds_band_inside_hysteresis() {
        reset_liquid_unplayed();
        assert_eq!(liquid_unplayed_after_one_second(0.9), LIQUID_UNPLAYED_DARK);
        assert_eq!(
            liquid_unplayed_after_one_second(0.5),
            LIQUID_UNPLAYED_DARK,
            "滞回区内不应换档"
        );

        reset_liquid_unplayed();
        assert_eq!(liquid_unplayed_after_one_second(0.1), LIQUID_UNPLAYED_LIGHT);
        assert_eq!(
            liquid_unplayed_after_one_second(0.5),
            LIQUID_UNPLAYED_LIGHT,
            "滞回区内不应换档"
        );
    }

    /// 换档是渐变而非硬切：单帧不会直接跳到目标灰度。
    #[test]
    fn liquid_unplayed_color_fades_instead_of_jumping() {
        reset_liquid_unplayed();
        let first_frame = liquid_unplayed_color(0.9, 1.0);
        assert!(
            first_frame.to_hsv().v > 0.6,
            "单帧就跳到深色档会显得生硬: {first_frame:?}"
        );
        assert_eq!(liquid_unplayed_after_one_second(0.9), LIQUID_UNPLAYED_DARK);
    }

    /// 采样缺失（`None` 由调用方回退）时不应把状态推进到目标档之外。
    #[test]
    fn liquid_unplayed_color_stays_within_band_bounds() {
        reset_liquid_unplayed();
        for luma in [0.0, 0.3, 0.45, 0.55, 0.7, 1.0] {
            for _ in 0..120 {
                let color = liquid_unplayed_color(luma, 1.0);
                let value = color.b();
                assert!(
                    value >= LIQUID_UNPLAYED_DARK.b() && value <= LIQUID_UNPLAYED_LIGHT.b(),
                    "灰度越出两档范围: {value}"
                );
            }
        }
    }

    /// 互补色之间的过渡必须保持色彩，不能退化成灰色。
    #[test]
    fn gradient_keeps_saturation_between_complementary_colors() {
        let colors =
            boundary_gradient_colors(Color::from_rgb(0, 0, 255), Color::from_rgb(255, 0, 0), 255);
        assert_eq!(colors.len(), HUE_RING_STEPS);
        let mid = colors[HUE_RING_STEPS / 2].to_hsv();
        // 蓝色 240° → 红色 0°：较短弧应经过 300°（品红）而不是绕道 120°（绿）
        assert!(
            (mid.h - 300.0).abs() <= 2.0,
            "色相没有沿较短弧过渡: {}",
            mid.h
        );
        assert!(
            mid.s > 0.95 && mid.v > 0.95,
            "中间色饱和度或明度不足，观感会发灰: {mid:?}"
        );
        // 对照：RGB 线性插值的中点明度只有一半
        let linear = Color::from_rgb(127, 0, 127).to_hsv();
        assert!(linear.v < mid.v - 0.4, "对照色本身就不发灰，用例失效");
    }

    /// 色相接近时不做色相环绕行，保持原算法的两色线性过渡。
    #[test]
    fn gradient_stays_linear_for_close_hues() {
        let colors = boundary_gradient_colors(
            Color::from_rgb(255, 128, 0),
            Color::from_rgb(255, 0, 0),
            255,
        );
        assert_eq!(
            colors,
            vec![Color::from_rgb(255, 128, 0), Color::from_rgb(255, 0, 0)],
            "邻近色相应保持线性过渡"
        );
    }

    /// 无彩色（黑白灰）参与时色相没有意义，退回两色线性过渡并保留透明度。
    #[test]
    fn gradient_falls_back_to_linear_for_achromatic_colors() {
        let colors = boundary_gradient_colors(Color::WHITE, Color::from_rgb(0, 0, 255), 200);
        assert_eq!(colors.len(), 2);
        assert_eq!(colors[0], Color::from_argb(200, 255, 255, 255));
        assert_eq!(colors[1], Color::from_argb(200, 0, 0, 255));

        // 灰色过渡到彩色同样按无彩色处理，不触发色相环
        let colors = boundary_gradient_colors(
            Color::from_rgb(128, 128, 128),
            Color::from_rgb(255, 0, 0),
            255,
        );
        assert_eq!(colors.len(), 2);
        assert_eq!(colors[0], Color::from_rgb(128, 128, 128));
        assert_eq!(colors[1], Color::from_rgb(255, 0, 0));
    }

    /// 按色相投票选主色：占比最大的色相胜出，近黑、近白、灰与半透明像素不参与投票。
    #[test]
    fn dominant_color_votes_by_hue() {
        // 红多于蓝时应选出红（BGRA 字节序）
        let mut pixels = Vec::new();
        for _ in 0..3 {
            pixels.extend_from_slice(&[0, 0, 255, 255]);
        }
        pixels.extend_from_slice(&[255, 0, 0, 255]);
        let picked = dominant_color_from_bgra(&pixels).expect("应能选出主色");
        let (hue, _, _) = rgb_to_hsl(
            f32::from(picked.r()),
            f32::from(picked.g()),
            f32::from(picked.b()),
        );
        assert!(hue < 20.0 || hue > 340.0, "应选出占比更高的红色相: {hue}");

        // 近黑、近白与灰像素都被过滤，纯灰封面取不到颜色
        let achromatic: Vec<u8> = [
            [200, 200, 200, 255],
            [30, 30, 30, 255],
            [128, 128, 128, 255],
            [250, 250, 250, 255],
        ]
        .into_iter()
        .flatten()
        .collect();
        assert_eq!(dominant_color_from_bgra(&achromatic), None);
        assert_eq!(dominant_color_from_bgra(&[]), None);
        // 半透明像素不参与投票
        assert_eq!(dominant_color_from_bgra(&[0, 0, 255, 100]), None);
    }

    /// 主色被规范化到深色背景上可读的区间，色相保持不变。
    #[test]
    fn normalize_played_color_clamps_saturation_and_lightness() {
        // 淡粉：饱和度 0.30 偏低、明度 0.80 偏高，两个方向都要被拉进区间
        let pale = Color::from_rgb(220, 190, 190);
        let normalized = normalize_played_color(pale);
        let (hue_before, _, _) = rgb_to_hsl(220.0, 190.0, 190.0);
        let (hue_after, saturation, lightness) = rgb_to_hsl(
            f32::from(normalized.r()),
            f32::from(normalized.g()),
            f32::from(normalized.b()),
        );
        assert!(
            (hue_before - hue_after).abs() < 2.0,
            "规范化不应改变色相: {hue_before} -> {hue_after}"
        );
        assert!(
            saturation >= PLAYED_SATURATION_RANGE.0 - 0.02,
            "偏淡的主色饱和度没有被提升: {saturation}"
        );
        assert!(
            lightness <= PLAYED_LIGHTNESS_RANGE.1 + 0.02,
            "过亮的主色明度没有被压低: {lightness}"
        );

        // 过艳的主色饱和度被压回上限
        let vivid = normalize_played_color(Color::from_rgb(0, 255, 0));
        let (_, saturation, _) = rgb_to_hsl(
            f32::from(vivid.r()),
            f32::from(vivid.g()),
            f32::from(vivid.b()),
        );
        assert!(
            saturation <= PLAYED_SATURATION_RANGE.1 + 0.02,
            "过艳的主色饱和度没有被压低: {saturation}"
        );

        // 灰色主色回落兜底色
        assert_eq!(
            normalize_played_color(Color::from_rgb(128, 128, 128)),
            PLAYED_FALLBACK_COLOR
        );
    }

    /// 封面图像走完整链路（缩放 → 读取像素 → 投票）能取到主色。
    #[test]
    fn cover_dominant_color_reads_image_pixels() {
        let mut surface = surfaces::raster_n32_premul(ISize::new(32, 32)).unwrap();
        surface.canvas().clear(Color::from_rgb(20, 120, 60));
        let picked = cover_dominant_color(&surface.image_snapshot()).expect("纯色封面应能取到主色");
        let (hue, _, _) = rgb_to_hsl(
            f32::from(picked.r()),
            f32::from(picked.g()),
            f32::from(picked.b()),
        );
        // (20, 120, 60) 的色相约 144°
        assert!((hue - 144.0).abs() < 6.0, "取到的色相不对: {hue}");
    }

    /// 端到端：彩色封面取到主色，黑白封面回落兜底色。
    #[test]
    fn pick_played_color_handles_chromatic_and_achromatic_covers() {
        let mut surface = surfaces::raster_n32_premul(ISize::new(32, 32)).unwrap();
        surface.canvas().clear(Color::from_rgb(200, 40, 40));
        let picked = pick_played_color(&surface.image_snapshot(), "chromatic");
        let (hue, _, _) = rgb_to_hsl(
            f32::from(picked.r()),
            f32::from(picked.g()),
            f32::from(picked.b()),
        );
        assert!(hue < 20.0 || hue > 340.0, "红色封面应取到红色: {hue}");

        let mut surface = surfaces::raster_n32_premul(ISize::new(32, 32)).unwrap();
        surface.canvas().clear(Color::from_rgb(180, 180, 180));
        assert_eq!(
            pick_played_color(&surface.image_snapshot(), "achromatic"),
            PLAYED_FALLBACK_COLOR
        );
    }

    /// HSL 与 RGB 往返一致，且灰度没有色相。
    #[test]
    fn hsl_conversion_round_trips() {
        for (r, g, b) in [
            (255.0, 0.0, 0.0),
            (0.0, 128.0, 0.0),
            (12.0, 34.0, 56.0),
            (200.0, 200.0, 200.0),
        ] {
            let (hue, saturation, lightness) = rgb_to_hsl(r, g, b);
            let color = hsl_to_color(hue, saturation, lightness);
            assert!(
                (f32::from(color.r()) - r).abs() <= 1.0
                    && (f32::from(color.g()) - g).abs() <= 1.0
                    && (f32::from(color.b()) - b).abs() <= 1.0,
                "HSL 往返后颜色变了: ({r}, {g}, {b}) -> {color:?}"
            );
        }
        let (hue, saturation, lightness) = rgb_to_hsl(128.0, 128.0, 128.0);
        assert_eq!((hue, saturation), (0.0, 0.0));
        assert!((lightness - 128.0 / 255.0).abs() < 1e-6);
    }
}
