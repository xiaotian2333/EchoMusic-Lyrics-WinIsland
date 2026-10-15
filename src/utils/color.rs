use std::cell::Cell;

use skia_safe::{Color, HSV, Point, TileMode, gradient_shader};

/// 饱和度低于该值的颜色视为无彩色，色相没有意义，插值时退回 RGB 线性过渡。
const ACHROMATIC_SATURATION: f32 = 0.05;
/// 色相环上相隔达到该角度即视为互补色一类：线性混合的中点会明显发灰，改走色相环。
const COMPLEMENTARY_HUE_SPAN: f32 = 120.0;
/// 色相环插值的取样点数，取奇数以便有一个精确的中点色。
const HUE_RING_STEPS: usize = 9;
/// 自动取色时饱和度低于该值即视为黑白灰，跳过该候选。
const PLAYED_MIN_SATURATION: f32 = 0.15;
/// 偏淡的封面颜色提升到的目标饱和度上限，避免粉彩配色被推得过艳。
const PLAYED_SATURATION_TARGET: f32 = 0.55;
/// 已播放色提升后的最低明度，保证在深色岛上清晰可读。
const PLAYED_MIN_VALUE: f32 = 0.75;
/// 封面确实只有黑白灰时的兜底浅蓝。
const PLAYED_FALLBACK_COLOR: Color = Color::from_rgb(120, 190, 255);

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
}

pub fn color_with_alpha(color: Color, alpha: u8) -> Color {
    Color::from_argb(alpha, color.r(), color.g(), color.b())
}

/// 逐字歌词已播放色的自动取值：从封面调色板中挑出第一支有色彩感的颜色。
///
/// 黑白灰（饱和度低于 [`PLAYED_MIN_SATURATION`]）一律跳过；挑中的颜色若偏淡，
/// 提升饱和度保证高亮可辨。整张封面只有黑白灰、或调色板为空时，返回浅蓝兜底色。
pub fn pick_played_color(palette: &[Color]) -> Color {
    palette
        .iter()
        .map(|color| color.to_hsv())
        .find(|hsv| hsv.s >= PLAYED_MIN_SATURATION)
        .map(|hsv| {
            HSV {
                h: hsv.h,
                s: hsv.s.max(PLAYED_SATURATION_TARGET),
                v: hsv.v.max(PLAYED_MIN_VALUE),
            }
            .to_color(255)
        })
        .unwrap_or(PLAYED_FALLBACK_COLOR)
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
        PLAYED_FALLBACK_COLOR, PLAYED_MIN_VALUE, PLAYED_SATURATION_TARGET,
        boundary_gradient_colors, liquid_unplayed_color, pick_played_color,
    };
    use skia_safe::Color;

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

    /// 封面只有黑白灰（或调色板为空）时使用浅蓝兜底，不能把黑白灰当成已播放色。
    #[test]
    fn played_color_falls_back_to_light_blue_for_achromatic_covers() {
        let achromatic = [
            Color::from_rgb(200, 200, 200),
            Color::from_rgb(30, 30, 30),
            Color::from_rgb(128, 128, 128),
        ];
        assert_eq!(pick_played_color(&achromatic), PLAYED_FALLBACK_COLOR);
        assert_eq!(pick_played_color(&[]), PLAYED_FALLBACK_COLOR);

        let fallback_hsv = PLAYED_FALLBACK_COLOR.to_hsv();
        assert!(
            fallback_hsv.s > 0.5,
            "兜底色必须是彩色而不是灰: {fallback_hsv:?}"
        );
    }

    /// 偏淡的封面颜色被挑中后要提升饱和度与明度，保证高亮清晰可辨。
    #[test]
    fn played_color_boosts_pale_cover_colors() {
        // 淡粉：饱和度约 0.22，够低算"偏淡"，又高于黑白灰筛选阈值
        let pale = Color::from_rgb(230, 180, 180);
        let picked = pick_played_color(&[pale]);
        let hsv = picked.to_hsv();
        assert!(
            hsv.s >= PLAYED_SATURATION_TARGET - 0.04,
            "偏淡颜色的饱和度没有被提升: {hsv:?}"
        );
        assert!(
            hsv.v >= PLAYED_MIN_VALUE - 0.01,
            "偏淡颜色的明度没有被提升: {hsv:?}"
        );
        let hue_before = pale.to_hsv().h;
        let hue_after = hsv.h;
        assert!(
            (hue_before - hue_after).abs() < 2.0,
            "提升饱和度时不应明显改变色相: {hue_before} -> {hue_after}"
        );
    }

    /// 调色板里黑白灰在前、彩色在后时，应跳过无彩色候选挑中第一支彩色。
    #[test]
    fn played_color_skips_achromatic_candidates() {
        let palette = [
            Color::from_rgb(250, 250, 250),
            Color::from_rgb(60, 60, 60),
            Color::from_rgb(20, 120, 60),
        ];
        let picked = pick_played_color(&palette);
        let target = Color::from_rgb(20, 120, 60).to_hsv();
        let hsv = picked.to_hsv();
        assert!(
            (hsv.h - target.h).abs() < 1.0,
            "没有挑中调色板里的彩色候选: {hsv:?}"
        );
        assert!(hsv.s >= PLAYED_SATURATION_TARGET - 0.04, "{hsv:?}");
    }
}
