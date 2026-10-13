//! 液态玻璃效果，渲染方式移植自 WinIsland：
//! GDI 截取窗口下方屏幕内容 → frost 模糊（降采样 + 可分离高斯）得到低分辨率背板 →
//! liquid.sksl 在背板上完成边缘折射、色散、镜面高光、描边与阴影。
//! 背板以 blur_scale 分之一的分辨率保存，着色器通过局部矩阵放大采样。

use std::cell::{Cell, RefCell};
use std::f32::consts::FRAC_1_SQRT_2;
use std::time::Instant;

use skia_safe::{
    AlphaType, BlendMode, Canvas, Color, ColorType, Data, FilterMode, ISize, Image, ImageInfo,
    Matrix, MipmapMode, Paint, Rect, RuntimeEffect, SamplingOptions, Surface, TileMode, images,
    runtime_effect::ChildPtr, surfaces,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{SetWindowDisplayAffinity, WINDOW_DISPLAY_AFFINITY};

use crate::utils::gdi_capture::{ScreenCaptureRequest, capture_screen_bgra};

/// 折射着色器源码（移植自 WinIsland 的 liquid.sksl）。
const LIQUID_SKSL_SOURCE: &str = include_str!("liquid.sksl");
/// frost 模糊着色器源码（移植自 WinIsland 的 frost.sksl）。
const FROST_SKSL_SOURCE: &str = include_str!("frost.sksl");

// 连续圆角（超椭圆）映射参数，与 WinIsland liquid.rs 保持一致。
const CORNER_SMOOTHING_SPAN: f32 = 3.333_333_3;
const CONTINUOUS_CORNER_EXPONENT: f32 = 2.5;
const CIRCULAR_APEX_INSET: f32 = 1.0 - FRAC_1_SQRT_2;
const MAX_BEZEL_FRACTION: f32 = 0.7;
const MAX_DISPERSION: f32 = 0.5;
const EDGE_REACH: f32 = 1.0;
const SHARP_RIM_REACH: f32 = 0.08;

// frost 模糊参数，与 WinIsland backdrop.rs 保持一致。
const MIN_FROST_SCALE: f32 = 2.0;
const MAX_FROST_SCALE: f32 = 24.0;
const MAX_WORK_SIGMA: f32 = 4.0;
const MIN_WORK_SIGMA: f32 = 0.3;
const MAX_WORK_SPAN: f32 = 12.0;

// 背板截图与最终帧的缓存有效期。
const CAPTURE_TTL_MS: u128 = 1000;
const FRAME_TTL_MS: u128 = 200;

/// 将岛屿窗口从 GDI 屏幕截图中排除（或恢复）。
///
/// 液态玻璃需要截取窗口下方内容，必须排除自身避免自捕获反馈。
/// 进入液态玻璃时设置，离开时清除。
pub fn set_exclude_from_capture(hwnd: HWND, exclude: bool) {
    // SAFETY: SetWindowDisplayAffinity 只修改调用方传入窗口句柄的显示亲和性。
    // 句柄由窗口层提供，调用失败只影响视觉捕获策略，返回值可忽略。
    unsafe {
        let _ = SetWindowDisplayAffinity(
            hwnd,
            if exclude {
                WINDOW_DISPLAY_AFFINITY(0x00000011)
            } else {
                WINDOW_DISPLAY_AFFINITY(0x00000000)
            },
        );
    }
}

/// 玻璃材质参数，移植自 WinIsland 的 LiquidMaterial。
#[derive(Clone, Copy)]
struct LiquidMaterial {
    bezel_width: f32,
    thickness: f32,
    glass_ior: f32,
    displacement_factor: f32,
    dispersion: f32,
    morph_dispersion: f32,
    tint: Color,
    legibility: f32,
    light: (f32, f32),
    specular_strength: f32,
    opposite_specular_strength: f32,
    specular_sharpness: f32,
    specular_falloff: f32,
    specular_opacity: f32,
    specular_width: f32,
    stroke_width: f32,
    stroke_opacity: f32,
    stroke_offset_y: f32,
    shadow_offset: (f32, f32),
    shadow_radius: f32,
    shadow_spread: f32,
    shadow_alpha: f32,
    black_fade: f32,
}

impl LiquidMaterial {
    /// 岛屿基础材质，参数随 scale 缩放（移植自 WinIsland LiquidMaterial::island）。
    fn island(scale: f32) -> Self {
        Self {
            bezel_width: 18.0 * scale,
            thickness: 14.0 * scale,
            glass_ior: 1.45,
            displacement_factor: 1.1,
            dispersion: 0.08,
            morph_dispersion: 0.06,
            tint: Color::from_argb(95, 10, 11, 15),
            legibility: 0.32,
            light: (-FRAC_1_SQRT_2, -FRAC_1_SQRT_2),
            specular_strength: 0.75,
            opposite_specular_strength: 0.38,
            specular_sharpness: 3.0,
            specular_falloff: 0.9,
            specular_opacity: 0.8,
            specular_width: 1.5 * scale,
            stroke_width: 0.65 * scale,
            stroke_opacity: 0.25,
            stroke_offset_y: 0.2 * scale,
            shadow_offset: (0.0, 2.0 * scale),
            shadow_radius: 7.0 * scale,
            shadow_spread: -scale,
            shadow_alpha: 0.22,
            black_fade: 0.0,
        }
    }

    /// 按折射强度缩放边缘位移，并设置色散强度（0..1，内部映射到最大值）。
    fn with_optics(mut self, refraction: f32, dispersion: f32) -> Self {
        self.displacement_factor *= refraction.max(0.0);
        self.dispersion = dispersion.clamp(0.0, 1.0) * MAX_DISPERSION;
        self
    }

    /// 按强度缩放暗色遮罩与自适应可读性着色；0 表示完全透明玻璃。
    fn with_tint(mut self, strength: f32) -> Self {
        let strength = strength.max(0.0);
        let alpha = (f32::from(self.tint.a()) * strength)
            .round()
            .clamp(0.0, 255.0) as u8;
        self.tint = self.tint.with_a(alpha);
        self.legibility *= strength;
        self
    }

    /// 阴影与描边超出形状边缘的最大距离，用于确定绘制范围。
    fn margin(&self) -> f32 {
        (self.shadow_radius + self.shadow_spread.max(0.0))
            .max(self.stroke_width + self.stroke_offset_y)
            + self.shadow_offset.0.abs().max(self.shadow_offset.1.abs())
            + 2.0
    }
}

/// 玻璃轮廓（画布坐标，画布即窗口内坐标）。
#[derive(Clone, Copy)]
struct LiquidShape {
    rect: Rect,
    radius: f32,
}

/// 模糊后的屏幕背板。
struct Backdrop {
    image: Image,
    /// 背板相对窗口的分辨率比例；1.0 表示未模糊。
    blur_scale: f32,
    /// 背板左上角在画布坐标中的位置（窗口部分离屏时非零）。
    origin: (f32, f32),
}

/// 背板截图缓存键：窗口与显示器矩形。
#[derive(Clone, Copy, PartialEq)]
struct CaptureKey {
    win_x: i32,
    win_y: i32,
    os_w: u32,
    os_h: u32,
    mon_x: i32,
    mon_y: i32,
    mon_w: u32,
    mon_h: u32,
}

/// 最终帧缓存键：几何、材质与模糊强度。
#[derive(Clone, Copy, PartialEq)]
struct FrameKey {
    offset_x: u32,
    offset_y: u32,
    width: u32,
    height: u32,
    radius: u32,
    scale: u32,
    sigma_key: u32,
    refraction: u32,
    dispersion: u32,
    tint: u32,
    siri: bool,
}

/// 背板截图缓存：(图像, 代次, 画布内原点, 时间, 键)。
type CaptureCacheEntry = (Image, u64, (f32, f32), Instant, CaptureKey);
/// frost 缓存：(图像, blur_scale, 背板代次, sigma 键)。
type FrostCacheEntry = (Image, f32, u64, u32);
/// 最终帧缓存：(图像, margin, 时间, 键)。
type FrameCacheEntry = (Image, f32, Instant, FrameKey);

thread_local! {
    static LIQUID_EFFECT: RefCell<Option<RuntimeEffect>> = const { RefCell::new(None) };
    static FROST_EFFECT: RefCell<Option<RuntimeEffect>> = const { RefCell::new(None) };
    static CAPTURE_CACHE: RefCell<Option<CaptureCacheEntry>> = const { RefCell::new(None) };
    static FROST_CACHE: RefCell<Option<FrostCacheEntry>> = const { RefCell::new(None) };
    static FRAME_CACHE: RefCell<Option<FrameCacheEntry>> = const { RefCell::new(None) };
    static CAPTURE_GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// 编译并缓存折射着色器。
fn liquid_effect() -> Option<RuntimeEffect> {
    LIQUID_EFFECT.with(|cell| {
        if let Some(effect) = cell.borrow().as_ref() {
            return Some(effect.clone());
        }
        let effect = RuntimeEffect::make_for_shader(LIQUID_SKSL_SOURCE, None).ok()?;
        *cell.borrow_mut() = Some(effect.clone());
        Some(effect)
    })
}

/// 编译并缓存 frost 模糊着色器。
fn frost_effect() -> Option<RuntimeEffect> {
    FROST_EFFECT.with(|cell| {
        if let Some(effect) = cell.borrow().as_ref() {
            return Some(effect.clone());
        }
        let effect = RuntimeEffect::make_for_shader(FROST_SKSL_SOURCE, None).ok()?;
        *cell.borrow_mut() = Some(effect.clone());
        Some(effect)
    })
}

/// 按名称写入 RuntimeEffect uniform 的辅助结构。
struct Uniforms<'a> {
    effect: &'a RuntimeEffect,
    bytes: Vec<u8>,
}

impl Uniforms<'_> {
    fn set(&mut self, name: &str, values: &[f32]) {
        let Some(uniform) = self.effect.find_uniform(name) else {
            return;
        };
        let offset = uniform.offset();
        for (index, value) in values.iter().enumerate() {
            let start = offset + index * 4;
            if let Some(slot) = self.bytes.get_mut(start..start + 4) {
                slot.copy_from_slice(&value.to_ne_bytes());
            }
        }
    }
}

/// 连续圆角映射：返回 [超椭圆范围, 指数]，移植自 WinIsland liquid.rs。
fn corner(rect: Rect, radius: f32) -> [f32; 2] {
    let limit = (rect.width().min(rect.height()) * 0.5).max(0.5);
    let radius = radius.clamp(0.5, limit);
    let extent =
        radius * CIRCULAR_APEX_INSET / (1.0 - 2.0_f32.powf(-1.0 / CONTINUOUS_CORNER_EXPONENT));
    if extent <= limit {
        return [extent, CONTINUOUS_CORNER_EXPONENT];
    }
    let exponent = (-(1.0 - radius * CIRCULAR_APEX_INSET / limit).log2())
        .recip()
        .clamp(2.0, 2.0 + CORNER_SMOOTHING_SPAN);
    [limit, exponent]
}

/// 形状 uniform：[中心 x, 中心 y, 半宽, 半高]。
fn shape_uniform(rect: Rect) -> [f32; 4] {
    [
        rect.center_x(),
        rect.center_y(),
        rect.width() * 0.5,
        rect.height() * 0.5,
    ]
}

/// 用折射着色器把背板绘制到画布上，返回是否成功。
///
/// 绘制范围覆盖形状四周 margin（阴影/描边），着色器自行处理边缘抗锯齿，
/// 调用方无需裁剪。progress 与 opacity 固定为 1.0（当前项目无融合动画）。
fn draw_liquid_glass(
    canvas: &Canvas,
    backdrop: &Backdrop,
    shape: &LiquidShape,
    material: &LiquidMaterial,
    progress: f32,
    opacity: f32,
) -> bool {
    let opacity = opacity.clamp(0.0, 1.0);
    let progress = progress.clamp(0.0, 1.0);
    if opacity <= 0.0 || progress <= 0.0 || shape.rect.width() <= 0.0 || shape.rect.height() <= 0.0
    {
        return true;
    }
    let Some(effect) = liquid_effect() else {
        return false;
    };
    let min_half = shape.rect.width().min(shape.rect.height()) * 0.5;
    let bezel = material
        .bezel_width
        .min(min_half * MAX_BEZEL_FRACTION)
        .max(1.0);
    let thickness = material.thickness * bezel / material.bezel_width.max(1.0);
    let light_length = material.light.0.hypot(material.light.1);
    let light = if light_length > 1e-4 {
        [
            material.light.0 / light_length,
            material.light.1 / light_length,
        ]
    } else {
        [1.0, 0.0]
    };

    let mut uniforms = Uniforms {
        effect: &effect,
        bytes: vec![0; effect.uniform_size()],
    };
    uniforms.set("shape0", &shape_uniform(shape.rect));
    uniforms.set("corner0", &corner(shape.rect, shape.radius));
    uniforms.set("bezelWidth", &[bezel]);
    uniforms.set("thickness", &[thickness]);
    uniforms.set("glassIor", &[material.glass_ior]);
    uniforms.set("displacementFactor", &[material.displacement_factor]);
    uniforms.set(
        "dispersion",
        &[material.dispersion.clamp(0.0, MAX_DISPERSION)],
    );
    uniforms.set(
        "morphDispersion",
        &[material.morph_dispersion.clamp(0.0, 0.5)],
    );
    uniforms.set("progress", &[progress]);
    uniforms.set(
        "tint",
        &[
            f32::from(material.tint.r()) / 255.0,
            f32::from(material.tint.g()) / 255.0,
            f32::from(material.tint.b()) / 255.0,
            f32::from(material.tint.a()) / 255.0,
        ],
    );
    uniforms.set("legibility", &[material.legibility.clamp(0.0, 1.0)]);
    uniforms.set("lightDir", &light);
    uniforms.set(
        "specular",
        &[
            material.specular_strength,
            material.opposite_specular_strength,
            material.specular_opacity,
            material.specular_falloff,
        ],
    );
    uniforms.set(
        "specularShape",
        &[material.specular_width, material.specular_sharpness],
    );
    uniforms.set(
        "stroke",
        &[
            material.stroke_width,
            material.stroke_opacity,
            material.stroke_offset_y,
        ],
    );
    uniforms.set(
        "shadow",
        &[
            material.shadow_offset.0,
            material.shadow_offset.1,
            material.shadow_radius,
            material.shadow_spread,
        ],
    );
    uniforms.set("shadowAlpha", &[material.shadow_alpha]);
    uniforms.set("opacity", &[opacity]);
    uniforms.set("blackFade", &[material.black_fade.clamp(0.0, 1.0)]);
    // 背板未模糊（blur_scale = 1）时缩短边缘折射采样范围，避免边缘抖动。
    let rim_reach = if backdrop.blur_scale > 1.0 {
        EDGE_REACH
    } else {
        (bezel * SHARP_RIM_REACH * material.displacement_factor).clamp(EDGE_REACH, bezel)
    };
    uniforms.set("rimReach", &[rim_reach]);

    let sampling = SamplingOptions::new(FilterMode::Linear, MipmapMode::None);
    // 局部矩阵：画布坐标 p → 背板纹理坐标 (p - origin) / blur_scale。
    let local_matrix = Matrix::new_all(
        backdrop.blur_scale,
        0.0,
        backdrop.origin.0,
        0.0,
        backdrop.blur_scale,
        backdrop.origin.1,
        0.0,
        0.0,
        1.0,
    );
    let Some(backdrop_shader) = backdrop.image.to_shader(
        (TileMode::Clamp, TileMode::Clamp),
        sampling,
        Some(&local_matrix),
    ) else {
        return false;
    };
    let children = [ChildPtr::from(backdrop_shader)];
    let Some(shader) = effect.make_shader(Data::new_copy(&uniforms.bytes), &children, None) else {
        return false;
    };

    let margin = material.margin();
    let mut paint = Paint::default();
    paint.set_shader(shader);
    canvas.draw_rect(
        Rect::from_ltrb(
            shape.rect.left - margin,
            shape.rect.top - margin,
            shape.rect.right + margin,
            shape.rect.bottom + margin,
        ),
        &paint,
    );
    true
}

/// frost 着色器的两遍处理模式。
enum FrostPass {
    /// 降采样（盒式平均）。
    Reduce(f32),
    /// 可分离高斯的一遍，参数为方向与工作分辨率下的 sigma。
    Blur([f32; 2], f32),
}

/// 在目标表面上执行一遍 frost 着色器，返回结果图像。
fn frost_pass(
    effect: &RuntimeEffect,
    source: &Image,
    target: &mut Surface,
    pass: FrostPass,
) -> Option<Image> {
    let sampling = SamplingOptions::new(FilterMode::Linear, MipmapMode::None);
    let source_shader = source.to_shader((TileMode::Clamp, TileMode::Clamp), sampling, None)?;
    let mut uniforms = Uniforms {
        effect,
        bytes: vec![0; effect.uniform_size()],
    };
    match pass {
        FrostPass::Reduce(scale) => uniforms.set("boxSize", &[scale]),
        FrostPass::Blur(direction, sigma) => {
            uniforms.set("direction", &direction);
            uniforms.set("sigma", &[sigma]);
            uniforms.set("span", &[(sigma * 3.0).ceil().min(MAX_WORK_SPAN)]);
        }
    }
    let shader = effect.make_shader(
        Data::new_copy(&uniforms.bytes),
        &[ChildPtr::from(source_shader)],
        None,
    )?;
    let mut paint = Paint::default();
    paint.set_shader(shader);
    paint.set_blend_mode(BlendMode::Src);
    let target_w = target.width() as f32;
    let target_h = target.height() as f32;
    target
        .canvas()
        .draw_rect(Rect::from_xywh(0.0, 0.0, target_w, target_h), &paint);
    Some(target.image_snapshot())
}

/// 降采样 + 可分离高斯模糊，返回背板与 blur_scale（移植自 WinIsland backdrop.rs）。
fn frost(capture: &Image, frost_sigma: f32) -> Option<(Image, f32)> {
    if frost_sigma < MIN_WORK_SIGMA {
        return Some((capture.clone(), 1.0));
    }
    let effect = frost_effect()?;
    let scale = (frost_sigma / MAX_WORK_SIGMA)
        .ceil()
        .clamp(MIN_FROST_SCALE, MAX_FROST_SCALE);
    let reduced_w = (capture.width() as f32 / scale).ceil().max(1.0) as i32;
    let reduced_h = (capture.height() as f32 / scale).ceil().max(1.0) as i32;
    let size = ISize::new(reduced_w, reduced_h);
    let mut reduce_surface = surfaces::raster_n32_premul(size)?;
    let reduced = frost_pass(
        &effect,
        capture,
        &mut reduce_surface,
        FrostPass::Reduce(scale),
    )?;
    let work_sigma = frost_sigma / scale;
    if work_sigma < MIN_WORK_SIGMA {
        return Some((reduced, scale));
    }
    let mut blur_surface = surfaces::raster_n32_premul(size)?;
    let horizontal = frost_pass(
        &effect,
        &reduced,
        &mut blur_surface,
        FrostPass::Blur([1.0, 0.0], work_sigma),
    )?;
    let mut blur_surface = surfaces::raster_n32_premul(size)?;
    let vertical = frost_pass(
        &effect,
        &horizontal,
        &mut blur_surface,
        FrostPass::Blur([0.0, 1.0], work_sigma),
    )?;
    Some((vertical, scale))
}

/// 获取或截取窗口下方屏幕内容，返回 (图像, 代次, 画布内原点)。
///
/// 截图范围裁剪到显示器内；窗口部分离屏时原点非零，着色器仍按窗口坐标采样。
fn get_or_capture(params: &LiquidGlassParams) -> Option<(Image, u64, (f32, f32))> {
    let key = CaptureKey {
        win_x: params.win_x,
        win_y: params.win_y,
        os_w: params.os_w,
        os_h: params.os_h,
        mon_x: params.monitor_x,
        mon_y: params.monitor_y,
        mon_w: params.monitor_w,
        mon_h: params.monitor_h,
    };
    let cached = CAPTURE_CACHE.with(|cell| {
        let cache = cell.borrow();
        if let Some((image, generation, origin, time, cached_key)) = cache.as_ref()
            && *cached_key == key
            && time.elapsed().as_millis() < CAPTURE_TTL_MS
        {
            return Some((image.clone(), *generation, *origin));
        }
        None
    });
    if let Some(hit) = cached {
        return Some(hit);
    }

    let left = params.win_x.max(params.monitor_x);
    let top = params.win_y.max(params.monitor_y);
    let right = (params.win_x + params.os_w as i32).min(params.monitor_x + params.monitor_w as i32);
    let bottom =
        (params.win_y + params.os_h as i32).min(params.monitor_y + params.monitor_h as i32);
    if right <= left || bottom <= top {
        return None;
    }
    let capture = capture_screen_bgra(ScreenCaptureRequest {
        src_x: left,
        src_y: top,
        src_w: right - left,
        src_h: bottom - top,
        dst_w: right - left,
        dst_h: bottom - top,
        use_halftone: false,
        force_opaque_alpha: true,
    })?;

    let info = ImageInfo::new(
        ISize::new(capture.width, capture.height),
        ColorType::BGRA8888,
        AlphaType::Premul,
        None,
    );
    let data = Data::new_copy(&capture.pixels);
    let image = images::raster_from_data(&info, data, (capture.width * 4) as usize)?;

    let generation = CAPTURE_GENERATION.with(|cell| {
        let next = cell.get().wrapping_add(1);
        cell.set(next);
        next
    });
    let origin = ((left - params.win_x) as f32, (top - params.win_y) as f32);
    CAPTURE_CACHE.with(|cell| {
        *cell.borrow_mut() = Some((image.clone(), generation, origin, Instant::now(), key));
    });
    Some((image, generation, origin))
}

/// 获取或生成 frost 模糊背板（按背板代次与 sigma 键缓存）。
fn get_or_frost(
    capture: &Image,
    generation: u64,
    sigma_key: u32,
    frost_sigma: f32,
) -> Option<(Image, f32)> {
    let cached = FROST_CACHE.with(|cell| {
        let cache = cell.borrow();
        if let Some((image, blur_scale, cached_generation, cached_sigma)) = cache.as_ref()
            && *cached_generation == generation
            && *cached_sigma == sigma_key
        {
            return Some((image.clone(), *blur_scale));
        }
        None
    });
    if let Some(hit) = cached {
        return Some(hit);
    }
    let (image, blur_scale) = frost(capture, frost_sigma)?;
    FROST_CACHE.with(|cell| {
        *cell.borrow_mut() = Some((image.clone(), blur_scale, generation, sigma_key));
    });
    Some((image, blur_scale))
}

/// 液态玻璃绘制参数。
pub struct LiquidGlassParams {
    /// 窗口左上角屏幕坐标。
    pub win_x: i32,
    pub win_y: i32,
    /// 窗口尺寸。
    pub os_w: u32,
    pub os_h: u32,
    /// 岛屿在窗口内的位置与尺寸（画布坐标）。
    pub offset_x: f32,
    pub offset_y: f32,
    pub width: f32,
    pub height: f32,
    pub corner_radius: f32,
    /// 当前视觉缩放（随展开进度在非展开/展开缩放间插值）。
    pub scale: f32,
    /// 背景模糊强度，frost sigma = blur * scale。
    pub blur: f32,
    /// 折射强度（配置值 / 100）。
    pub refraction: f32,
    /// 色散强度（配置值 / 100）。
    pub dispersion: f32,
    /// 暗色遮罩强度（配置值 / 100）。
    pub tint: f32,
    /// Siri 样式：黑色到玻璃的渐变。
    pub siri: bool,
    /// 所在显示器矩形（截图裁剪边界）。
    pub monitor_x: i32,
    pub monitor_y: i32,
    pub monitor_w: u32,
    pub monitor_h: u32,
}

/// 最终帧：带 margin 的玻璃图像与绘制偏移。
pub struct LiquidGlassFrame {
    pub image: Image,
    pub margin: f32,
}

/// 获取液态玻璃帧；失败时调用方应回退到实心圆角矩形。
pub fn get_liquid_glass_background(params: &LiquidGlassParams) -> Option<LiquidGlassFrame> {
    if params.width <= 0.0 || params.height <= 0.0 || params.os_w == 0 || params.os_h == 0 {
        return None;
    }
    let frost_sigma = params.blur * params.scale;
    let sigma_key = (frost_sigma * 16.0).round() as u32;

    let mut material = LiquidMaterial::island(params.scale)
        .with_optics(params.refraction, params.dispersion)
        .with_tint(params.tint);
    if params.siri {
        material.black_fade = 1.0;
    }
    let shape = LiquidShape {
        rect: Rect::from_xywh(
            params.offset_x,
            params.offset_y,
            params.width,
            params.height,
        ),
        radius: params.corner_radius,
    };

    let frame_key = FrameKey {
        offset_x: params.offset_x.to_bits(),
        offset_y: params.offset_y.to_bits(),
        width: params.width.to_bits(),
        height: params.height.to_bits(),
        radius: params.corner_radius.to_bits(),
        scale: params.scale.to_bits(),
        sigma_key,
        refraction: params.refraction.to_bits(),
        dispersion: params.dispersion.to_bits(),
        tint: params.tint.to_bits(),
        siri: params.siri,
    };

    // 帧缓存：几何与材质不变时直接复用，避免每帧重跑着色器。
    let cached = FRAME_CACHE.with(|cell| {
        let cache = cell.borrow();
        if let Some((image, margin, time, key)) = cache.as_ref()
            && *key == frame_key
            && time.elapsed().as_millis() < FRAME_TTL_MS
        {
            return Some((image.clone(), *margin));
        }
        None
    });
    if let Some((image, margin)) = cached {
        return Some(LiquidGlassFrame { image, margin });
    }

    let (capture, generation, origin) = get_or_capture(params)?;
    let (backdrop_image, blur_scale) = get_or_frost(&capture, generation, sigma_key, frost_sigma)?;
    let backdrop = Backdrop {
        image: backdrop_image,
        blur_scale,
        origin,
    };

    let margin = material.margin().ceil();
    let out_w = (params.width + margin * 2.0).ceil() as i32;
    let out_h = (params.height + margin * 2.0).ceil() as i32;
    if out_w <= 0 || out_h <= 0 {
        return None;
    }
    let mut surface = surfaces::raster_n32_premul(ISize::new(out_w, out_h))?;
    {
        let canvas = surface.canvas();
        canvas.clear(Color::TRANSPARENT);
        // 平移画布，使着色器收到的局部坐标与窗口坐标一致（背板采样依赖该对齐）。
        canvas.translate((-(shape.rect.left - margin), -(shape.rect.top - margin)));
        if !draw_liquid_glass(canvas, &backdrop, &shape, &material, 1.0, 1.0) {
            return None;
        }
    }
    let image = surface.image_snapshot();

    FRAME_CACHE.with(|cell| {
        *cell.borrow_mut() = Some((image.clone(), margin, Instant::now(), frame_key));
    });
    Some(LiquidGlassFrame { image, margin })
}

/// 清空所有液态玻璃缓存（样式切换、主题变化或配置重载时调用）。
pub fn clear_liquid_glass_cache() {
    CAPTURE_CACHE.with(|cell| {
        *cell.borrow_mut() = None;
    });
    FROST_CACHE.with(|cell| {
        *cell.borrow_mut() = None;
    });
    FRAME_CACHE.with(|cell| {
        *cell.borrow_mut() = None;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 折射着色器必须能编译通过。
    #[test]
    fn liquid_shader_compiles() {
        assert!(RuntimeEffect::make_for_shader(LIQUID_SKSL_SOURCE, None).is_ok());
    }

    /// frost 着色器必须能编译通过。
    #[test]
    fn frost_shader_compiles() {
        assert!(RuntimeEffect::make_for_shader(FROST_SKSL_SOURCE, None).is_ok());
    }

    /// 连续圆角范围不超过矩形短边的一半，指数保持连续圆角默认值。
    #[test]
    fn corner_extent_stays_within_limit() {
        let rect = Rect::from_xywh(0.0, 0.0, 200.0, 100.0);
        let [extent, exponent] = corner(rect, 24.0);
        assert!(extent > 0.0 && extent <= 50.0);
        assert_eq!(exponent, CONTINUOUS_CORNER_EXPONENT);
    }

    /// 光学参数与遮罩强度按范围收敛。
    #[test]
    fn optics_and_tint_clamp() {
        let material = LiquidMaterial::island(1.0)
            .with_optics(-1.0, 2.0)
            .with_tint(0.0);
        assert_eq!(material.displacement_factor, 0.0);
        assert_eq!(material.dispersion, MAX_DISPERSION);
        assert_eq!(material.tint.a(), 0);
        assert_eq!(material.legibility, 0.0);
    }

    /// 折射着色器能在 CPU 光栅画布上完整走通（编译 + 绘制）。
    #[test]
    fn liquid_glass_draws_on_raster_canvas() {
        let mut backdrop_surface = surfaces::raster_n32_premul(ISize::new(32, 32)).unwrap();
        backdrop_surface
            .canvas()
            .clear(Color::from_argb(255, 128, 128, 128));
        let backdrop = Backdrop {
            image: backdrop_surface.image_snapshot(),
            blur_scale: 1.0,
            origin: (0.0, 0.0),
        };
        let mut surface = surfaces::raster_n32_premul(ISize::new(64, 64)).unwrap();
        let shape = LiquidShape {
            rect: Rect::from_xywh(8.0, 8.0, 48.0, 48.0),
            radius: 12.0,
        };
        let material = LiquidMaterial::island(1.0)
            .with_optics(2.0, 0.4)
            .with_tint(1.0);
        assert!(draw_liquid_glass(
            surface.canvas(),
            &backdrop,
            &shape,
            &material,
            1.0,
            1.0,
        ));
    }

    /// frost 着色器能在 CPU 光栅画布上完整走通（降采样 + 单遍模糊）。
    #[test]
    fn frost_shader_draws_on_raster_canvas() {
        let effect = frost_effect().unwrap();
        let mut source_surface = surfaces::raster_n32_premul(ISize::new(16, 16)).unwrap();
        source_surface
            .canvas()
            .clear(Color::from_argb(255, 100, 150, 200));
        let source = source_surface.image_snapshot();
        let mut target = surfaces::raster_n32_premul(ISize::new(8, 8)).unwrap();
        let reduced = frost_pass(&effect, &source, &mut target, FrostPass::Reduce(2.0)).unwrap();
        let mut target = surfaces::raster_n32_premul(ISize::new(8, 8)).unwrap();
        assert!(
            frost_pass(
                &effect,
                &reduced,
                &mut target,
                FrostPass::Blur([1.0, 0.0], 1.5)
            )
            .is_some()
        );
    }
}
