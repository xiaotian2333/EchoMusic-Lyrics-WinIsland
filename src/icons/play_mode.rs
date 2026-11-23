use skia_safe::{Canvas, Color, Paint};

// Tabler Icons — stroke-based SVG paths (viewport 24×24)
const REPEAT_OFF_PATH: &str = "M4 12V9a3 3 0 0 1 2.08-2.856M10 6h10m-3-3l3 3l-3 3m3 3v3a3 3 0 0 1-.133.886m-1.99 1.984A3 3 0 0 1 17 18H4m3 3l-3-3l3-3M3 3l18 18";
const REPEAT_PATH: &str =
    "M4 12V9a3 3 0 0 1 3-3h13m-3-3l3 3l-3 3m3 3v3a3 3 0 0 1-3 3H4m3 3l-3-3l3-3";
const SHUFFLE_PATH: &str = "M18 4l3 3l-3 3m0 10l3-3l-3-3M3 7h3a5 5 0 0 1 5 5a5 5 0 0 0 5 5h5m0-10h-5a4.978 4.978 0 0 0-3 1m-4 8a4.984 4.984 0 0 1-3 1H3";
const REPEAT_ONCE_PATH: &str =
    "M4 12V9a3 3 0 0 1 3-3h13m-3-3l3 3l-3 3m3 3v3a3 3 0 0 1-3 3H4m3 3l-3-3l3-3m4-4l1-1v4";

pub fn draw_play_mode_icon(
    canvas: &Canvas,
    cx: f32,
    cy: f32,
    alpha: u8,
    scale: f32,
    color: Color,
    mode: &str,
) {
    let path_data = match mode {
        "sequential" => REPEAT_OFF_PATH,
        "list" => REPEAT_PATH,
        "random" => SHUFFLE_PATH,
        "single" => REPEAT_ONCE_PATH,
        _ => return,
    };

    let Some(path) = super::cached_svg_path(path_data) else {
        return;
    };

    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(alpha, color.r(), color.g(), color.b()));
    paint.set_style(skia_safe::PaintStyle::Stroke);
    paint.set_stroke_width(1.8);
    paint.set_stroke_cap(skia_safe::paint::Cap::Round);
    paint.set_stroke_join(skia_safe::paint::Join::Round);

    canvas.save();
    canvas.translate((cx, cy));
    let s = 1.5 * scale;
    canvas.scale((s, s));
    canvas.translate((-12.0, -12.0));
    canvas.draw_path(&path, &paint);
    canvas.restore();
}
