use skia_safe::{Canvas, Color, Paint};

const PLAY_TRIANGLE_PATH: &str = "M187.2 100.9C174.8 94.1 159.8 94.4 147.6 101.6C135.4 108.8 128 121.9 128 136L128 504C128 518.1 135.5 531.2 147.6 538.4C159.7 545.6 174.8 545.9 187.2 539.1L523.2 355.1C536 348.1 544 334.6 544 320C544 305.4 536 291.9 523.2 284.9L187.2 100.9z";

pub fn draw_play_button(canvas: &Canvas, cx: f32, cy: f32, alpha: u8, scale: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(alpha, color.r(), color.g(), color.b()));
    paint.set_style(skia_safe::paint::Style::Fill);

    if let Some(path) = super::cached_svg_path(PLAY_TRIANGLE_PATH) {
        canvas.save();
        canvas.translate((cx, cy));
        let s = 0.083 * scale;
        canvas.scale((s, s));
        canvas.translate((-320.0, -320.0));
        canvas.draw_path(&path, &paint);
        canvas.restore();
    }
}

pub fn draw_pause_button(canvas: &Canvas, cx: f32, cy: f32, alpha: u8, scale: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(alpha, color.r(), color.g(), color.b()));
    paint.set_style(skia_safe::paint::Style::Fill);

    let path_data = "M176 96C149.5 96 128 117.5 128 144L128 496C128 522.5 149.5 544 176 544L240 544C266.5 544 288 522.5 288 496L288 144C288 117.5 266.5 96 240 96L176 96zM400 96C373.5 96 352 117.5 352 144L352 496C352 522.5 373.5 544 400 544L464 544C490.5 544 512 522.5 512 496L512 144C512 117.5 490.5 96 464 96L400 96z";

    if let Some(path) = super::cached_svg_path(path_data) {
        canvas.save();
        canvas.translate((cx, cy));
        let s = 0.083 * scale;
        canvas.scale((s, s));
        canvas.translate((-320.0, -320.0));
        canvas.draw_path(&path, &paint);
        canvas.restore();
    }
}

pub fn draw_control_triangle(
    canvas: &Canvas,
    cx: f32,
    cy: f32,
    alpha: u8,
    path_scale: f32,
    scale: f32,
    color: Color,
) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(alpha, color.r(), color.g(), color.b()));
    paint.set_style(skia_safe::paint::Style::Fill);

    if let Some(path) = super::cached_svg_path(PLAY_TRIANGLE_PATH) {
        canvas.save();
        canvas.translate((cx, cy));
        let s = path_scale * scale;
        canvas.scale((s, s));
        canvas.translate((-336.0, -320.0));
        canvas.draw_path(&path, &paint);
        canvas.restore();
    }
}

pub fn draw_star_button(canvas: &Canvas, cx: f32, cy: f32, alpha: u8, scale: f32, color: Color) {
    let mut paint = Paint::default();
    paint.set_anti_alias(true);
    paint.set_color(Color::from_argb(alpha, color.r(), color.g(), color.b()));
    paint.set_style(skia_safe::paint::Style::Fill);

    let path_data = "M288 24L354.4 179.2L522.2 193.8L395.4 304L433.4 468.2L288 383.2L142.6 468.2L180.6 304L53.8 193.8L221.6 179.2Z";

    if let Some(path) = super::cached_svg_path(path_data) {
        canvas.save();
        canvas.translate((cx, cy));
        let s = 0.062 * scale;
        canvas.scale((s, s));
        canvas.translate((-288.0, -256.0));
        canvas.draw_path(&path, &paint);
        canvas.restore();
    }
}
