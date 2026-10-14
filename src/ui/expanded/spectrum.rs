//! WinIsland 原版频谱可视化（移植自 WinIsland 的
//! `ui/expanded/music_view/visualizer.rs` 与 `music_view/palette.rs`）。
//!
//! 频谱填充有两种来源：
//! - `Cover`：从专辑封面按 2x3 网格提取主色（OKLab/LCh 加权直方图），再插值成
//!   16x16 的色块图；频谱条以自身圆角矩形为裁剪区，从这张色块图上取对应切片，
//!   得到「取自封面的渐变」。
//! - `Colors`：没有封面时使用固定的三段灰阶，跨整排频谱做对角线性渐变。

use std::cell::RefCell;
use std::collections::HashMap;

use skia_safe::{
    AlphaType, Canvas, ClipOp, Color, ColorType, Data, FilterMode, ISize, Image, ImageInfo,
    MipmapMode, Paint, Point, RRect, Rect, SamplingOptions, TileMode, gradient_shader, images,
    surfaces,
};

use crate::core::media_info::MediaInfo;
use crate::ui::expanded::music_view::get_cached_media_image_with_key;

/// 封面采样分辨率。
const SAMPLE_SIZE: i32 = 24;
/// 主色网格列数与行数。
const GRID_COLUMNS: usize = 2;
const GRID_ROWS: usize = 3;
/// 色块图分辨率。
const SPECTRUM_IMAGE_SIZE: i32 = 16;
/// 色相分箱数与中性色分箱数。
const HUE_BINS: usize = 12;
const NEUTRAL_BINS: usize = 3;
/// 低于该彩度按中性色处理，高于该彩度按鲜艳度加权。
const NEUTRAL_CHROMA: f32 = 0.04;
const VIBRANCE_CHROMA: f32 = 0.02;
/// 主色亮度下限与相对背板的对比度要求。
const MIN_LIGHTNESS: f32 = 0.5;
const MIN_CONTRAST: f32 = 3.0;
const BACKDROP_DIM: f32 = 120.0 / 255.0;
const BACKDROP_SHADE: f32 = 20.0;
const CACHE_LIMIT: usize = 50;

thread_local! {
    static SPECTRUM_CACHE: RefCell<HashMap<String, Option<Image>>> = RefCell::new(HashMap::new());
    static VIZ_HEIGHTS: RefCell<[f32; 6]> = const { RefCell::new([3.0; 6]) };
}

/// 频谱条的填充来源。
#[derive(Clone)]
pub enum SpectrumFill {
    /// 取自专辑封面的 16x16 色块图。
    Cover(Image),
    /// 无封面时的固定配色，按对角渐变铺满整排频谱。
    Colors(Vec<Color>),
}

pub struct DrawVisualizerParams<'a> {
    pub canvas: &'a Canvas,
    pub x: f32,
    pub y: f32,
    pub alpha: u8,
    pub is_playing: bool,
    pub fill: &'a SpectrumFill,
    pub spectrum: &'a [f32; 6],
    pub w_scale: f32,
    pub h_scale: f32,
    pub smooth_factors: (f32, f32),
}

/// 当前曲目的频谱填充；有封面时用封面主色网格，否则用默认配色。
pub fn get_media_spectrum_fill(media: &MediaInfo) -> SpectrumFill {
    get_cached_media_image_with_key(media)
        .and_then(|(img, cache_key)| get_spectrum_image(&img, &cache_key))
        .map_or_else(default_spectrum_fill, SpectrumFill::Cover)
}

/// 无封面时的默认配色（与 WinIsland 一致的三段灰阶）。
pub fn default_spectrum_fill() -> SpectrumFill {
    SpectrumFill::Colors(vec![
        Color::from_rgb(176, 176, 184),
        Color::from_rgb(206, 206, 212),
        Color::from_rgb(236, 236, 240),
    ])
}

pub fn clear_spectrum_cache() {
    SPECTRUM_CACHE.with(|cache| cache.borrow_mut().clear());
}

/// 绘制频谱：六根圆角竖条，高度按频谱值做上升/下降双速平滑。
pub fn draw_visualizer(params: DrawVisualizerParams<'_>) {
    let DrawVisualizerParams {
        canvas,
        x,
        y,
        alpha,
        is_playing,
        fill,
        spectrum,
        w_scale,
        h_scale,
        smooth_factors,
    } = params;

    let (rise, fall) = smooth_factors;
    let bar_count = 6;
    let bar_w = 3.0 * w_scale;
    let spacing = 2.0 * w_scale;
    let max_h = 28.0 * h_scale;
    VIZ_HEIGHTS.with(|h_cell| {
        let mut heights = h_cell.borrow_mut();
        for i in 0..bar_count {
            let target = if is_playing {
                (spectrum[i] * max_h).max(3.0 * h_scale)
            } else {
                3.0 * h_scale
            };
            if target > heights[i] {
                heights[i] = heights[i] * (1.0 - rise) + target * rise;
            } else {
                heights[i] = heights[i] * (1.0 - fall) + target * fall;
            }
            heights[i] = heights[i].max(3.0 * h_scale);
        }
        let start_x = x - (bar_count as f32 * (bar_w + spacing)) / 2.0;
        let span = bar_count as f32 * (bar_w + spacing) - spacing;
        let bounds = Rect::from_xywh(start_x, y - max_h / 2.0, span, max_h);
        let radius = bar_w / 2.0;
        let bars = (0..bar_count).map(|i| {
            let h = heights[i];
            Rect::from_xywh(
                start_x + i as f32 * (bar_w + spacing),
                y - h / 2.0,
                bar_w,
                h,
            )
        });
        match fill {
            SpectrumFill::Cover(image) => {
                let mut paint = Paint::default();
                paint.set_anti_alias(true);
                paint.set_alpha_f(f32::from(alpha) / 255.0);
                let sampling = SamplingOptions::new(FilterMode::Linear, MipmapMode::None);
                for rect in bars {
                    canvas.save();
                    canvas.clip_rrect(
                        RRect::new_rect_xy(rect, radius, radius),
                        ClipOp::Intersect,
                        true,
                    );
                    canvas.draw_image_rect_with_sampling_options(
                        image, None, bounds, sampling, &paint,
                    );
                    canvas.restore();
                }
            }
            SpectrumFill::Colors(colors) => {
                let mut paint = Paint::default();
                paint.set_anti_alias(true);
                let colors_with_alpha: Vec<Color> = colors
                    .iter()
                    .map(|c| Color::from_argb(alpha, c.r(), c.g(), c.b()))
                    .collect();
                if colors_with_alpha.len() >= 2 {
                    if let Some(shader) = gradient_shader::linear(
                        (
                            Point::new(bounds.left, bounds.top),
                            Point::new(bounds.right, bounds.bottom),
                        ),
                        colors_with_alpha.as_slice(),
                        None,
                        TileMode::Clamp,
                        None,
                        None,
                    ) {
                        paint.set_shader(shader);
                    }
                } else {
                    paint.set_color(
                        colors_with_alpha
                            .first()
                            .copied()
                            .unwrap_or(Color::from_argb(alpha, 255, 255, 255)),
                    );
                }
                for rect in bars {
                    canvas.draw_round_rect(rect, radius, radius, &paint);
                }
            }
        }
    });
}

/// 取（并缓存）封面主色网格图。
fn get_spectrum_image(img: &Image, cache_key: &str) -> Option<Image> {
    SPECTRUM_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(image) = cache.get(cache_key) {
            return image.clone();
        }
        if cache.len() >= CACHE_LIMIT
            && let Some(oldest) = cache.keys().next().cloned()
        {
            cache.remove(&oldest);
        }
        let image = spectrum_image(img);
        cache.insert(cache_key.to_string(), image.clone());
        image
    })
}

/// 把封面降采样成 24x24，按 2x3 网格取主色后插值成 16x16 色块图。
fn spectrum_image(image: &Image) -> Option<Image> {
    if image.width() <= 0 || image.height() <= 0 {
        return None;
    }
    let sample = SAMPLE_SIZE as usize;
    let info = ImageInfo::new(
        ISize::new(SAMPLE_SIZE, SAMPLE_SIZE),
        ColorType::BGRA8888,
        AlphaType::Premul,
        None,
    );
    let mut surface = surfaces::raster(&info, None, None)?;
    surface.canvas().draw_image_rect_with_sampling_options(
        image,
        None,
        Rect::from_wh(SAMPLE_SIZE as f32, SAMPLE_SIZE as f32),
        SamplingOptions::new(FilterMode::Linear, MipmapMode::None),
        &Paint::default(),
    );
    let mut pixels = vec![0u8; sample * sample * 4];
    if !surface.read_pixels(&info, &mut pixels, info.min_row_bytes(), (0, 0)) {
        return None;
    }
    let rgb: Vec<[f32; 3]> = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| {
            // BGRA 预乘像素先还原成直通色，避免半透明封面把颜色压暗
            let alpha = (f32::from(pixel[3]) / 255.0).max(f32::EPSILON);
            [pixel[2], pixel[1], pixel[0]].map(|channel| (f32::from(channel) / alpha).min(255.0))
        })
        .collect();
    let colors: Vec<Lab> = rgb
        .iter()
        .map(|&[red, green, blue]| oklab(red, green, blue))
        .collect();
    let floor = lightness_floor(&rgb);
    let (cell_width, cell_height) = (sample / GRID_COLUMNS, sample / GRID_ROWS);
    let blocks: Vec<Lab> = (0..GRID_COLUMNS * GRID_ROWS)
        .map(|index| {
            let (column, row) = (index % GRID_COLUMNS, index / GRID_COLUMNS);
            let block = (row * cell_height..(row + 1) * cell_height).flat_map(|y| {
                (column * cell_width..(column + 1) * cell_width).map(move |x| y * sample + x)
            });
            lift(dominant_color(block.map(|pixel| colors[pixel])), floor)
        })
        .collect();
    let size = SPECTRUM_IMAGE_SIZE as usize;
    let position = |value: usize| (value as f32 + 0.5) / size as f32;
    let mut rgba = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        for x in 0..size {
            let color = srgb(mesh_color(&blocks, position(x), position(y)));
            rgba.extend_from_slice(&[color.r(), color.g(), color.b(), 255]);
        }
    }
    let out_info = ImageInfo::new(
        ISize::new(SPECTRUM_IMAGE_SIZE, SPECTRUM_IMAGE_SIZE),
        ColorType::RGBA8888,
        AlphaType::Unpremul,
        None,
    );
    images::raster_from_data(
        &out_info,
        Data::new_copy(&rgba),
        (SPECTRUM_IMAGE_SIZE * 4) as usize,
    )
}

/// 网格块主色：按色相/中性分箱统计，彩度越高权重越大，取权重最大的分箱均值。
fn dominant_color(pixels: impl Iterator<Item = Lab>) -> Lab {
    let mut bins = [(0.0f32, [0.0f32; 3]); HUE_BINS + NEUTRAL_BINS];
    for color in pixels {
        let lch = color.lch();
        let bin = if lch.c < NEUTRAL_CHROMA {
            HUE_BINS + ((lch.l * NEUTRAL_BINS as f32) as usize).min(NEUTRAL_BINS - 1)
        } else {
            let turn = lch.h.rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
            ((turn * HUE_BINS as f32) as usize).min(HUE_BINS - 1)
        };
        let weight = 1.0 + lch.c / VIBRANCE_CHROMA;
        let (total, sum) = &mut bins[bin];
        *total += weight;
        sum[0] += color.l * weight;
        sum[1] += color.a * weight;
        sum[2] += color.b * weight;
    }
    let (total, sum) = bins
        .into_iter()
        .max_by(|left, right| left.0.total_cmp(&right.0))
        .unwrap_or_default();
    let total = total.max(f32::EPSILON);
    Lab {
        l: sum[0] / total,
        a: sum[1] / total,
        b: sum[2] / total,
    }
}

/// 主色亮度下限：按封面平均色（压暗后）推算，保证频谱在深色背板上仍然可见。
fn lightness_floor(rgb: &[[f32; 3]]) -> f32 {
    let count = rgb.len().max(1) as f32;
    let sum = rgb.iter().fold([0.0f32; 3], |sum, pixel| {
        [sum[0] + pixel[0], sum[1] + pixel[1], sum[2] + pixel[2]]
    });
    let [red, green, blue] =
        sum.map(|channel| channel / count * (1.0 - BACKDROP_DIM) + BACKDROP_SHADE * BACKDROP_DIM);
    let backdrop = oklab(red, green, blue).l.powi(3);
    (MIN_CONTRAST * (backdrop + 0.05) - 0.05)
        .cbrt()
        .max(MIN_LIGHTNESS)
}

fn lift(color: Lab, floor: f32) -> Lab {
    Lab {
        l: color.l.max(floor),
        ..color
    }
}

/// 在 2x3 网格上做双线性（smoothstep）插值，得到平滑的色块图采样。
fn mesh_color(blocks: &[Lab], x: f32, y: f32) -> Lab {
    let axis = |value: f32, count: usize| {
        let position = (value * count as f32 - 0.5).clamp(0.0, (count - 1) as f32);
        let index = (position as usize).min(count - 2);
        let fraction = position - index as f32;
        (index, fraction * fraction * (3.0 - 2.0 * fraction))
    };
    let mix = |from: Lab, to: Lab, amount: f32| Lab {
        l: from.l + (to.l - from.l) * amount,
        a: from.a + (to.a - from.a) * amount,
        b: from.b + (to.b - from.b) * amount,
    };
    let ((column, horizontal), (row, vertical)) = (axis(x, GRID_COLUMNS), axis(y, GRID_ROWS));
    let block = |column: usize, row: usize| blocks[row * GRID_COLUMNS + column];
    mix(
        mix(block(column, row), block(column + 1, row), horizontal),
        mix(
            block(column, row + 1),
            block(column + 1, row + 1),
            horizontal,
        ),
        vertical,
    )
}

#[derive(Clone, Copy)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

#[derive(Clone, Copy)]
struct Lch {
    l: f32,
    c: f32,
    h: f32,
}

impl Lab {
    fn lch(self) -> Lch {
        Lch {
            l: self.l,
            c: self.a.hypot(self.b),
            h: self.b.atan2(self.a),
        }
    }
}

impl Lch {
    fn lab(self) -> Lab {
        Lab {
            l: self.l,
            a: self.c * self.h.cos(),
            b: self.c * self.h.sin(),
        }
    }
}

fn srgb_to_linear(channel: f32) -> f32 {
    let channel = channel / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(channel: f32) -> f32 {
    let channel = if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    (channel * 255.0).clamp(0.0, 255.0)
}

fn oklab(red: f32, green: f32, blue: f32) -> Lab {
    let (r, g, b) = (
        srgb_to_linear(red),
        srgb_to_linear(green),
        srgb_to_linear(blue),
    );
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    Lab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

fn linear_rgb(color: Lab) -> [f32; 3] {
    let l = (color.l + 0.396_337_78 * color.a + 0.215_803_76 * color.b).powi(3);
    let m = (color.l - 0.105_561_346 * color.a - 0.063_854_17 * color.b).powi(3);
    let s = (color.l - 0.089_484_18 * color.a - 1.291_485_5 * color.b).powi(3);
    [
        4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s,
        -1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s,
        -0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s,
    ]
}

/// OKLab 转 sRGB；超出色域时按 LCh 二分降低彩度直到落入色域。
fn srgb(color: Lab) -> Color {
    let in_gamut = |rgb: [f32; 3]| rgb.iter().all(|channel| (-0.001..=1.001).contains(channel));
    let mut rgb = linear_rgb(color);
    if !in_gamut(rgb) {
        let lch = color.lch();
        let (mut low, mut high) = (0.0, lch.c);
        for _ in 0..16 {
            let chroma = (low + high) / 2.0;
            if in_gamut(linear_rgb(Lch { c: chroma, ..lch }.lab())) {
                low = chroma;
            } else {
                high = chroma;
            }
        }
        rgb = linear_rgb(Lch { c: low, ..lch }.lab());
    }
    Color::from_rgb(
        linear_to_srgb(rgb[0]) as u8,
        linear_to_srgb(rgb[1]) as u8,
        linear_to_srgb(rgb[2]) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        DrawVisualizerParams, GRID_COLUMNS, GRID_ROWS, Lab, SPECTRUM_IMAGE_SIZE, SpectrumFill,
        default_spectrum_fill, dominant_color, draw_visualizer, lightness_floor, oklab,
        spectrum_image, srgb,
    };
    use skia_safe::{
        AlphaType, Color, ColorType, Data, ISize, Image, ImageInfo, image::CachingHint, images,
        surfaces,
    };

    /// 读取图像指定像素的 RGB（测试用）。
    fn read_rgb(image: &Image, x: i32, y: i32) -> (u8, u8, u8) {
        let info = ImageInfo::new(
            ISize::new(1, 1),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let mut pixel = [0u8; 4];
        assert!(
            image.read_pixels(&info, &mut pixel, 4, (x, y), CachingHint::Allow),
            "读取像素失败"
        );
        (pixel[0], pixel[1], pixel[2])
    }

    /// 读取整张图像（测试用）。
    fn read_all(image: &Image, size: i32) -> Vec<u8> {
        let info = ImageInfo::new(
            ISize::new(size, size),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let mut pixels = vec![0u8; (size * size * 4) as usize];
        assert!(
            image.read_pixels(
                &info,
                &mut pixels,
                (size * 4) as usize,
                (0, 0),
                CachingHint::Allow
            ),
            "读取像素失败"
        );
        pixels
    }

    #[test]
    fn oklab_round_trip_keeps_primary_colors() {
        for &(r, g, b) in &[(255.0, 0.0, 0.0), (0.0, 128.0, 0.0), (0.0, 0.0, 255.0)] {
            let back = srgb(oklab(r, g, b));
            assert!(
                (i32::from(back.r()) - r as i32).abs() <= 2,
                "红通道往返失真"
            );
            assert!(
                (i32::from(back.g()) - g as i32).abs() <= 2,
                "绿通道往返失真"
            );
            assert!(
                (i32::from(back.b()) - b as i32).abs() <= 2,
                "蓝通道往返失真"
            );
        }
    }

    #[test]
    fn dominant_color_prefers_vivid_hue() {
        // 一个偏红的鲜艳块加一个低彩度灰块，主色应落在红附近
        let mut pixels = vec![oklab(220.0, 40.0, 40.0); 8];
        pixels.extend(vec![oklab(128.0, 128.0, 128.0); 8]);
        let color = dominant_color(pixels.into_iter());
        assert!(color.a > 0.0, "主色应保留暖色分量");
    }

    #[test]
    fn lightness_floor_keeps_colors_readable() {
        let dark = vec![[0.0f32, 0.0, 0.0]; GRID_COLUMNS * GRID_ROWS];
        let bright = vec![[255.0f32, 255.0, 255.0]; GRID_COLUMNS * GRID_ROWS];
        assert!(lightness_floor(&dark) >= 0.5);
        assert!(lightness_floor(&bright) >= lightness_floor(&dark));
        // 亮度下限应足以让纯黑封面也得到可见的频谱色
        assert!(
            Lab {
                l: 0.0,
                a: 0.0,
                b: 0.0
            }
            .l < lightness_floor(&dark)
        );
    }

    /// 左红右蓝的封面应生成左红右蓝的色块图，验证采样与网格插值链路。
    #[test]
    fn spectrum_image_keeps_cover_layout() {
        let size = 32i32;
        let mut rgba = Vec::with_capacity((size * size * 4) as usize);
        for _ in 0..size {
            for x in 0..size {
                let red = x < size / 2;
                rgba.extend_from_slice(&[
                    if red { 220 } else { 20 },
                    if red { 30 } else { 60 },
                    if red { 40 } else { 220 },
                    255,
                ]);
            }
        }
        let info = ImageInfo::new(
            ISize::new(size, size),
            ColorType::RGBA8888,
            AlphaType::Unpremul,
            None,
        );
        let cover = images::raster_from_data(&info, Data::new_copy(&rgba), (size * 4) as usize)
            .expect("测试封面创建失败");
        let spectrum = spectrum_image(&cover).expect("色块图生成失败");
        assert_eq!(
            (spectrum.width(), spectrum.height()),
            (SPECTRUM_IMAGE_SIZE, SPECTRUM_IMAGE_SIZE)
        );

        let (left_r, _, left_b) = read_rgb(&spectrum, 1, SPECTRUM_IMAGE_SIZE / 2);
        let (right_r, _, right_b) =
            read_rgb(&spectrum, SPECTRUM_IMAGE_SIZE - 2, SPECTRUM_IMAGE_SIZE / 2);
        assert!(left_r > left_b, "左侧应偏向红色: {left_r}/{left_b}");
        assert!(right_b > right_r, "右侧应偏向蓝色: {right_r}/{right_b}");
    }

    /// 两种填充都能在 CPU 光栅画布上画出非空频谱条。
    #[test]
    fn visualizer_draws_on_raster_canvas() {
        for fill in [
            default_spectrum_fill(),
            SpectrumFill::Cover({
                let info = ImageInfo::new(
                    ISize::new(4, 4),
                    ColorType::RGBA8888,
                    AlphaType::Unpremul,
                    None,
                );
                images::raster_from_data(&info, Data::new_copy(&[200u8; 4 * 4 * 4]), 16)
                    .expect("测试色块图创建失败")
            }),
        ] {
            let mut surface = surfaces::raster_n32_premul(ISize::new(64, 64)).unwrap();
            surface.canvas().clear(Color::TRANSPARENT);
            draw_visualizer(DrawVisualizerParams {
                canvas: surface.canvas(),
                x: 32.0,
                y: 32.0,
                alpha: 255,
                is_playing: true,
                fill: &fill,
                spectrum: &[1.0; 6],
                w_scale: 1.0,
                h_scale: 1.0,
                smooth_factors: (0.6, 0.08),
            });
            let pixels = read_all(&surface.image_snapshot(), 64);
            assert!(
                pixels.chunks_exact(4).any(|px| px[3] > 0),
                "频谱条未绘制任何像素"
            );
        }
    }
}
