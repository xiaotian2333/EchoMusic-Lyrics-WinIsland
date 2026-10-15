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
        HUE_RING_STEPS, PLAYED_FALLBACK_COLOR, PLAYED_MIN_VALUE, PLAYED_SATURATION_TARGET,
        boundary_gradient_colors, pick_played_color,
    };
    use skia_safe::Color;

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
