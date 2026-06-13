use skia_safe::Color;

pub fn get_island_border_weights(_cx: i32, _cy: i32, _w: f32, _h: f32) -> [f32; 4] {
    [0.0, 0.0, 0.0, 0.0]
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
