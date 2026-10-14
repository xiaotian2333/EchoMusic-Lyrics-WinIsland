//! WinIsland 原版毛玻璃宿主背板（移植自 WinIsland 的
//! `winisland-platform-windows/src/backdrop.rs` 与 `backdrop/blur.rs`）。
//!
//! 与液态玻璃不同，毛玻璃不截屏：伴随窗口开启 DWM 宿主背板
//! （`DWMWA_USE_HOSTBACKDROPBRUSH`）后，`Windows.UI.Composition` 的
//! `CreateHostBackdropBrush` 直接在窗口下方合成真实桌面内容，再按岛屿圆角矩形
//! 裁剪；可选的 D2D 高斯模糊提供磨砂程度。主岛窗口只需要在裁剪区域内绘制
//! 半透明暗色覆盖层。
//!
//! 独立成 crate 的原因：`#[implement]` 宏展开使用 `::windows_core` 绝对路径，
//! 而主程序仍需 `windows` 0.58 的 `#[implement]`（音频设备通知），
//! 同一 crate 内无法同时绑定两个 `windows_core` 版本。

mod blur;
mod styles;

use std::cell::{Cell, OnceCell, RefCell};

use windows::System::DispatcherQueueController;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionBrush, CompositionGeometricClip, CompositionRoundedRectangleGeometry, Compositor,
    ContainerVisual, SpriteVisual,
};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    SetWindowPos,
};
use windows::core::Interface;
use windows_numerics::{Vector2, Vector3};

use blur::BackdropBlur;
use styles::enable_host_backdrop;

/// 背板相对岛屿边缘的内缩，避免抗锯齿边缘处露出宿主背板。
const HOST_BACKDROP_INSET: f32 = 1.0;

/// 毛玻璃背板形状：屏幕像素坐标下的圆角矩形。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HostBackdropShape {
    pub screen_x: f32,
    pub screen_y: f32,
    pub width: f32,
    pub height: f32,
    pub radius: f32,
}

/// 合成器与其调度队列控制器；调度队列必须在 UI 线程创建并保持存活。
struct BackdropCompositionContext {
    _dispatcher_queue: DispatcherQueueController,
    compositor: Compositor,
}

/// 挂在合成根节点下、按圆角矩形几何裁剪的背板视觉。
struct ClippedVisual {
    visual: SpriteVisual,
    _clip: CompositionGeometricClip,
    geometry: CompositionRoundedRectangleGeometry,
}

/// 已提交给合成器的形状，用于跳过重复更新。
#[derive(Clone, Copy, PartialEq)]
struct ShapeGeometry {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
}

/// 已提交给合成器的窗口几何，用于跳过重复更新。
#[derive(Clone, Copy, PartialEq)]
struct BackdropGeometry {
    screen_x: i32,
    screen_y: i32,
    window_width: i32,
    window_height: i32,
    shape: ShapeGeometry,
}

thread_local! {
    static BACKDROP_COMPOSITION: OnceCell<Option<BackdropCompositionContext>> =
        const { OnceCell::new() };
}

/// 把尺寸量化到 1/16 像素，避免亚像素抖动导致合成器反复重建。
fn quantize(value: f32) -> f32 {
    (value * 16.0).round() / 16.0
}

/// 宿主背板渲染器：持有伴随窗口上的合成视觉树与背板画笔。
pub struct HostBackdrop {
    main_hwnd: HWND,
    backdrop_hwnd: HWND,
    target: DesktopWindowTarget,
    _root: ContainerVisual,
    primary: ClippedVisual,
    last_geometry: RefCell<Option<BackdropGeometry>>,
    compositor: Compositor,
    backdrop_brush: CompositionBrush,
    blur: OnceCell<Option<BackdropBlur>>,
    blur_amount: Cell<f32>,
}

impl HostBackdrop {
    /// 在伴随窗口上建立宿主背板合成树。
    ///
    /// `main_hwnd` 只用于把伴随窗口压在主岛窗口正下方，保持 z 序。
    /// 两个句柄都必须是本线程创建的存活窗口。
    pub fn new(main_hwnd: isize, backdrop_hwnd: isize) -> Result<Self, String> {
        let main_hwnd = HWND(main_hwnd as *mut core::ffi::c_void);
        let backdrop_hwnd = HWND(backdrop_hwnd as *mut core::ffi::c_void);
        let compositor =
            backdrop_compositor().ok_or_else(|| "Windows 宿主背板合成器不可用".to_string())?;
        if !enable_host_backdrop(backdrop_hwnd) {
            return Err("DWM 宿主背板画笔启用失败".to_string());
        }
        let interop: ICompositorDesktopInterop = compositor
            .cast()
            .map_err(|error| format!("ICompositorDesktopInterop 不可用: {error}"))?;
        // SAFETY: backdrop_hwnd 属于本进程的伴随窗口，且没有其他合成树占用该窗口目标。
        let target = unsafe { interop.CreateDesktopWindowTarget(backdrop_hwnd, false) }
            .map_err(|error| format!("CreateDesktopWindowTarget 失败: {error}"))?;
        let root = compositor
            .CreateContainerVisual()
            .map_err(|error| format!("CreateContainerVisual 失败: {error}"))?;
        let brush: CompositionBrush = compositor
            .CreateHostBackdropBrush()
            .and_then(|brush| brush.cast())
            .map_err(|error| format!("CreateHostBackdropBrush 失败: {error}"))?;
        let primary = ClippedVisual::new(&compositor, &brush, &root)?;
        target
            .SetRoot(&root)
            .map_err(|error| format!("宿主背板根节点挂载失败: {error}"))?;
        log::info!("毛玻璃宿主背板已启用（DWM 宿主背板画笔 + DirectComposition）");
        Ok(Self {
            main_hwnd,
            backdrop_hwnd,
            target,
            _root: root,
            primary,
            last_geometry: RefCell::new(None),
            compositor,
            backdrop_brush: brush,
            blur: OnceCell::new(),
            blur_amount: Cell::new(0.0),
        })
    }

    /// 更新背板形状与磨砂程度；`shape` 为 None 时隐藏背板窗口。
    pub fn update(&self, shape: Option<HostBackdropShape>, blur: f32) -> Result<(), String> {
        self.set_blur(blur)
            .map_err(|error| format!("毛玻璃模糊设置失败: {error}"))?;
        let shape = shape.filter(|shape| shape.width > 0.0 && shape.height > 0.0);
        let Some(shape) = shape else {
            self.hide();
            return Ok(());
        };
        let screen_x = shape.screen_x.floor();
        let screen_y = shape.screen_y.floor();
        let geometry = BackdropGeometry {
            screen_x: screen_x as i32,
            screen_y: screen_y as i32,
            window_width: ((shape.screen_x + shape.width).ceil() - screen_x).max(1.0) as i32,
            window_height: ((shape.screen_y + shape.height).ceil() - screen_y).max(1.0) as i32,
            shape: ShapeGeometry {
                x: quantize(shape.screen_x - screen_x + HOST_BACKDROP_INSET),
                y: quantize(shape.screen_y - screen_y + HOST_BACKDROP_INSET),
                width: quantize((shape.width - HOST_BACKDROP_INSET * 2.0).max(0.0)),
                height: quantize((shape.height - HOST_BACKDROP_INSET * 2.0).max(0.0)),
                radius: quantize((shape.radius - HOST_BACKDROP_INSET).max(0.0)),
            },
        };
        if self.last_geometry.borrow().as_ref() != Some(&geometry) {
            self.primary
                .apply(Some(geometry.shape))
                .map_err(|error| format!("宿主背板几何更新失败: {error}"))?;
            *self.last_geometry.borrow_mut() = Some(geometry);
        }
        // 主岛窗口每帧重新置顶，这里同步把背板压回主窗口正下方
        self.place_window(&geometry);
        Ok(())
    }

    /// 隐藏背板窗口与视觉。
    pub fn hide(&self) {
        if self.last_geometry.borrow_mut().take().is_none() {
            return;
        }
        let _ = self.primary.visual.SetIsVisible(false);
        // SAFETY: 背板 HWND 由伴随窗口持有，这里只隐藏它，不改变位置与 z 序。
        let _ = unsafe {
            SetWindowPos(
                self.backdrop_hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_HIDEWINDOW | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER,
            )
        };
    }

    /// 设置磨砂程度；0 表示只使用宿主背板本身（不加额外模糊）。
    fn set_blur(&self, amount: f32) -> windows::core::Result<()> {
        let amount = quantize(amount.max(0.0));
        let previous = self.blur_amount.get();
        if amount == previous {
            return Ok(());
        }
        let blur = if amount > 0.0 {
            self.blur
                .get_or_init(|| {
                    BackdropBlur::new(&self.compositor, &self.backdrop_brush)
                        .map_err(|error| log::warn!("毛玻璃模糊效果不可用: {error}"))
                        .ok()
                })
                .as_ref()
        } else {
            None
        };
        if let Some(blur) = blur {
            blur.set_amount(amount)?;
        }
        // 在「有模糊」与「无模糊」之间切换时需要换掉视觉上的画笔
        if (previous > 0.0) != (amount > 0.0) {
            let brush = match blur {
                Some(blur) => blur.brush()?,
                None => self.backdrop_brush.clone(),
            };
            self.primary.visual.SetBrush(&brush)?;
        }
        self.blur_amount.set(amount);
        Ok(())
    }

    /// 把伴随窗口贴着主窗口正下方摆放并显示。
    fn place_window(&self, geometry: &BackdropGeometry) {
        // SAFETY: 两个 HWND 都属于本线程存活的窗口；把背板窗口插入主窗口之后
        // （即 z 序更靠下）不会激活窗口，也不会改变主窗口位置与尺寸。
        let _ = unsafe {
            SetWindowPos(
                self.backdrop_hwnd,
                Some(self.main_hwnd),
                geometry.screen_x,
                geometry.screen_y,
                geometry.window_width,
                geometry.window_height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
        };
    }
}

impl Drop for HostBackdrop {
    fn drop(&mut self) {
        self.hide();
        let _ = self.target.Close();
    }
}

impl ClippedVisual {
    fn new(
        compositor: &Compositor,
        brush: &CompositionBrush,
        root: &ContainerVisual,
    ) -> Result<Self, String> {
        let visual = compositor
            .CreateSpriteVisual()
            .map_err(|error| format!("CreateSpriteVisual 失败: {error}"))?;
        let geometry = compositor
            .CreateRoundedRectangleGeometry()
            .map_err(|error| format!("CreateRoundedRectangleGeometry 失败: {error}"))?;
        let clip = compositor
            .CreateGeometricClipWithGeometry(&geometry)
            .map_err(|error| format!("CreateGeometricClipWithGeometry 失败: {error}"))?;
        visual
            .SetBrush(brush)
            .map_err(|error| format!("宿主背板画笔挂载失败: {error}"))?;
        visual
            .SetClip(&clip)
            .map_err(|error| format!("宿主背板裁剪挂载失败: {error}"))?;
        visual
            .SetIsVisible(false)
            .map_err(|error| format!("宿主背板可见性初始化失败: {error}"))?;
        root.Children()
            .and_then(|children| children.InsertAtTop(&visual))
            .map_err(|error| format!("宿主背板视觉插入失败: {error}"))?;
        Ok(Self {
            visual,
            _clip: clip,
            geometry,
        })
    }

    fn apply(&self, shape: Option<ShapeGeometry>) -> windows::core::Result<()> {
        let Some(shape) = shape else {
            return self.visual.SetIsVisible(false);
        };
        self.visual.SetOffset(Vector3 {
            X: shape.x,
            Y: shape.y,
            Z: 0.0,
        })?;
        let size = Vector2 {
            X: shape.width,
            Y: shape.height,
        };
        self.visual.SetSize(size)?;
        self.geometry.SetSize(size)?;
        self.geometry.SetCornerRadius(Vector2 {
            X: shape.radius,
            Y: shape.radius,
        })?;
        self.visual.SetIsVisible(true)
    }
}

fn backdrop_compositor() -> Option<Compositor> {
    BACKDROP_COMPOSITION.with(|cell| {
        cell.get_or_init(|| match create_backdrop_composition_context() {
            Ok(context) => Some(context),
            Err(error) => {
                log::warn!("Windows 宿主背板初始化失败: {error}");
                None
            }
        })
        .as_ref()
        .map(|context| context.compositor.clone())
    })
}

fn create_backdrop_composition_context() -> Result<BackdropCompositionContext, String> {
    let options = DispatcherQueueOptions {
        dwSize: size_of::<DispatcherQueueOptions>() as u32,
        threadType: DQTYPE_THREAD_CURRENT,
        apartmentType: DQTAT_COM_NONE,
    };
    // SAFETY: winit 在创建窗口前已初始化主线程的 STA；这里只在当前线程挂一个
    // 调度队列，不改变其 COM 单元模型，返回的控制器由上下文持有。
    let dispatcher_queue = unsafe { CreateDispatcherQueueController(options) }
        .map_err(|error| format!("CreateDispatcherQueueController 失败: {error}"))?;
    let compositor = Compositor::new().map_err(|error| format!("Compositor 创建失败: {error}"))?;
    Ok(BackdropCompositionContext {
        _dispatcher_queue: dispatcher_queue,
        compositor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    /// 冒烟测试：宿主背板合成器、DWM 宿主背板画笔与手写的高斯模糊效果对象。
    /// 该测试需要交互式桌面会话，默认忽略；手动运行：
    /// cargo test -p host-backdrop host_backdrop_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn host_backdrop_smoke() {
        // SAFETY: 测试线程独占，初始化 COM 后只使用合成 API，不再切换单元模型。
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
        let compositor = backdrop_compositor().expect("宿主背板合成器初始化失败");
        let brush: CompositionBrush = compositor
            .CreateHostBackdropBrush()
            .and_then(|brush| brush.cast())
            .expect("DWM 宿主背板画笔创建失败");
        let blur = BackdropBlur::new(&compositor, &brush).expect("高斯模糊效果对象创建失败");
        blur.set_amount(12.0).expect("模糊量写入失败");
        drop(blur);
        drop(brush);
        println!("宿主背板冒烟测试通过：合成器、宿主背板画笔与高斯模糊效果均可用");
    }
}
