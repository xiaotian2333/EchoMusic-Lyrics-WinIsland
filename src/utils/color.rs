use skia_safe::{Color, HSV, Point, TileMode, gradient_shader};

/// 饱和度低于该值的颜色视为无彩色，色相没有意义，插值时退回 RGB 线性过渡。
const ACHROMATIC_SATURATION: f32 = 0.05;
/// 色相环插值的取样点数，取奇数以便有一个精确的中点色。
const HUE_RING_STEPS: usize = 9;

pub fn color_with_alpha(color: Color, alpha: u8) -> Color {
    Color::from_argb(alpha, color.r(), color.g(), color.b())
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
/// 直接按 RGB 线性插值时，互补色（如蓝与红、红与青）的中点会退化成灰色；
/// 这里改为沿 HSV 色相环走较短弧，饱和度与明度线性插值，保证过渡途中始终有色彩。
/// 任一色接近无彩色（例如默认的白色未播放色）时色相没有意义，退回 RGB 线性过渡。
fn boundary_gradient_colors(played: Color, unplayed: Color, alpha: u8) -> Vec<Color> {
    let from = played.to_hsv();
    let to = unplayed.to_hsv();
    if from.s < ACHROMATIC_SATURATION || to.s < ACHROMATIC_SATURATION {
        return vec![
            color_with_alpha(played, alpha),
            color_with_alpha(unplayed, alpha),
        ];
    }
    // 超过半圈时反向绕行，始终取较短弧
    let mut delta = to.h - from.h;
    if delta > 180.0 {
        delta -= 360.0;
    } else if delta < -180.0 {
        delta += 360.0;
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

#[cfg(test)]
mod tests {
    use super::{HUE_RING_STEPS, boundary_gradient_colors};
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

    /// 色相接近时仍应近似线性过渡，不引入突兀的绕行。
    #[test]
    fn gradient_stays_near_linear_for_close_hues() {
        let colors = boundary_gradient_colors(
            Color::from_rgb(255, 128, 0),
            Color::from_rgb(255, 0, 0),
            255,
        );
        let mid = colors[HUE_RING_STEPS / 2].to_hsv();
        assert!(
            (mid.h - 15.0).abs() <= 3.0,
            "邻近色相应近似线性过渡: {}",
            mid.h
        );
    }

    /// 无彩色参与时色相没有意义，退回两色线性过渡并保留透明度。
    #[test]
    fn gradient_falls_back_to_linear_for_achromatic_colors() {
        let colors = boundary_gradient_colors(Color::WHITE, Color::from_rgb(0, 0, 255), 200);
        assert_eq!(colors.len(), 2);
        assert_eq!(colors[0], Color::from_argb(200, 255, 255, 255));
        assert_eq!(colors[1], Color::from_argb(200, 0, 0, 255));
    }
}
