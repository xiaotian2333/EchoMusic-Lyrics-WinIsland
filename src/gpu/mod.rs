//! WinIsland 原版 GPU 渲染管线（移植自 WinIsland 的 winisland-render 与
//! winisland-platform-windows）。
//!
//! 当岛样式为 liquid_glass 时启用：D3D12 设备 + Skia Ganesh D3D12 后端 +
//! DirectComposition 合成交换链负责整帧渲染；Windows.Graphics.Capture 捕获窗口下方
//! 屏幕内容，经共享纹理与围栏同步后在 GPU 上完成 frost 模糊与液态玻璃折射。
//!
//! 当岛样式为 glass（毛玻璃）时启用 `host_backdrop`：伴随窗口开启 DWM 宿主背板后，
//! 由 Windows.UI.Composition 的宿主背板画笔在窗口下方合成真实桌面内容并裁剪到岛屿
//! 形状，主岛窗口仍走 softbuffer 光栅路径。
//!
//! 其余样式（default/mica/dynamic）继续使用 softbuffer 的 CPU 光栅路径；
//! GPU 初始化失败时自动回退到 CPU 液态玻璃实现（utils::liquid_glass），
//! 宿主背板不可用时回退到 CPU 毛玻璃实现（utils::glass）。

mod backdrop;
mod capture;
mod device;
mod target;

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use skia_safe::Canvas;
use winit::event_loop::EventLoopProxy;

use crate::utils::liquid_glass::Backdrop as LiquidBackdrop;

pub(crate) use backdrop::SharedBackdrop;
pub(crate) use capture::BackdropCapture;
pub(crate) use device::D3DDevice;
// 毛玻璃宿主背板由独立 crate 提供：其 #[implement] 需要 windows-core 0.62，
// 而本 crate 的音频设备通知仍需 windows-core 0.58，两者无法在同一 crate 内共存
pub(crate) use host_backdrop::{HostBackdrop, HostBackdropShape};
pub(crate) use target::RenderTarget;

/// 背板捕获创建失败后的重试间隔。
const CAPTURE_RETRY: Duration = Duration::from_secs(10);

/// 背板共享纹理的代次计数，每次重建递增，供捕获端识别失效句柄。
static BACKDROP_GENERATION: AtomicU64 = AtomicU64::new(1);

static WAKE_PROXY: OnceLock<EventLoopProxy<()>> = OnceLock::new();
static WAKE_PENDING: AtomicBool = AtomicBool::new(false);

/// 注册事件循环唤醒代理（在 main.rs 中调用）。
pub fn set_wake_proxy(proxy: EventLoopProxy<()>) {
    let _ = WAKE_PROXY.set(proxy);
}

/// 从任意线程唤醒主事件循环（捕获回调使用）；重复调用合并为一次事件。
pub(crate) fn wake() {
    if WAKE_PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Some(proxy) = WAKE_PROXY.get() {
        let _ = proxy.send_event(());
    }
}

/// 清除唤醒挂起标记（主线程处理唤醒事件时调用）。
pub fn take_wake_pending() -> bool {
    WAKE_PENDING.swap(false, Ordering::AcqRel)
}

/// 屏幕像素矩形（对应 WinIsland 平台层的 Rect）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// 屏幕像素点。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Point {
    pub x: i32,
    pub y: i32,
}

/// 渲染器暴露给捕获端的共享纹理与围栏句柄。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackdropShare {
    pub texture: usize,
    pub write_fence: usize,
    pub read_fence: usize,
    pub width: u32,
    pub height: u32,
    pub generation: u64,
}

/// 背板捕获请求；矩形为屏幕像素，read_value 是渲染器最近一次读完成信号值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackdropCaptureRequest {
    pub share: BackdropShare,
    pub origin: Point,
    pub interest: Rect,
    pub read_value: u64,
    pub max_rate_hz: u32,
}

/// 背板更新结果；渲染器须在 write_fence 上等待 write_value 后再采样共享纹理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackdropCaptureFrame {
    pub write_value: u64,
    pub serial: u64,
}

/// GPU 岛渲染器：整合 D3D12 设备、合成交换链与背板捕获。
pub(crate) struct GpuIsland {
    hwnd_raw: isize,
    device: D3DDevice,
    target: RenderTarget,
    backdrop: Option<SharedBackdrop>,
    capture: Option<BackdropCapture>,
    capture_retry_at: Option<Instant>,
}

impl GpuIsland {
    /// 创建 GPU 渲染器；失败时调用方应回退到 CPU 路径并稍后重试。
    pub(crate) fn new(hwnd_raw: isize, width: u32, height: u32) -> Result<Self, String> {
        let mut device = D3DDevice::new()?;
        let target = device.create_target(hwnd_raw, width.max(1), height.max(1))?;
        log::info!("GPU 渲染管线已启用（D3D12 + DirectComposition 合成交换链）");
        Ok(Self {
            hwnd_raw,
            device,
            target,
            backdrop: None,
            capture: None,
            capture_retry_at: None,
        })
    }

    /// 窗口尺寸变化时重建交换链缓冲。
    pub(crate) fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.device.resize_target(&mut self.target, width, height)
    }

    /// 渲染一帧：更新背板捕获，在合成交换链上执行 draw 回调并呈现。
    ///
    /// draw 回调收到窗口大小的画布与可选 GPU 背板；背板为 None（尚未捕获成功）时
    /// 液态玻璃分支会自动回退到 CPU 截屏实现。
    pub(crate) fn render(
        &mut self,
        window_pos: (i32, i32),
        size: (u32, u32),
        frost_sigma: f32,
        animating: bool,
        draw: impl FnOnce(&Canvas, Option<&LiquidBackdrop>) -> bool,
    ) -> Result<bool, String> {
        self.device.check_health()?;
        if self.target.size() != size {
            self.resize(size.0, size.1)?;
        }
        let share = self.ensure_backdrop(size)?;
        let frame = self.update_capture(window_pos, size, share, animating);
        if let Some(backdrop) = self.backdrop.as_mut() {
            backdrop.set_frame(self.device.queue(), frame, frost_sigma)?;
        }
        self.draw_frame(draw)
    }

    /// 确保共享背板纹理存在且足够大，返回其共享句柄。
    fn ensure_backdrop(&mut self, size: (u32, u32)) -> Result<BackdropShare, String> {
        if let Some(backdrop) = &self.backdrop
            && backdrop.fits(size.0, size.1)
        {
            return Ok(backdrop.share());
        }
        if self.backdrop.is_some() {
            // 释放旧背板前等待 GPU 完成，避免捕获端仍在使用
            self.device.synchronize()?;
            self.backdrop = None;
        }
        let generation = BACKDROP_GENERATION.fetch_add(1, Ordering::Relaxed);
        let backdrop = self.device.create_backdrop(size.0, size.1, generation)?;
        let share = backdrop.share();
        self.backdrop = Some(backdrop);
        Ok(share)
    }

    /// 更新捕获会话并请求一帧背板拷贝；失败返回 None（下一帧或冷却后重试）。
    fn update_capture(
        &mut self,
        window_pos: (i32, i32),
        size: (u32, u32),
        share: BackdropShare,
        animating: bool,
    ) -> Option<BackdropCaptureFrame> {
        if self.capture.is_none() {
            let ready = self
                .capture_retry_at
                .is_none_or(|at| at.elapsed() >= CAPTURE_RETRY);
            if !ready {
                return None;
            }
            match BackdropCapture::new(self.hwnd_raw, self.device.adapter_luid()) {
                Ok(capture) => {
                    self.capture = Some(capture);
                    self.capture_retry_at = None;
                }
                Err(error) => {
                    log::warn!("屏幕捕获不可用，液态玻璃将使用 CPU 截屏背板: {error}");
                    self.capture_retry_at = Some(Instant::now());
                    return None;
                }
            }
        }
        let capture = self.capture.as_ref()?;
        // 捕获范围取整个窗口矩形，避免在此重复岛屿布局计算
        let interest = Rect {
            left: window_pos.0,
            top: window_pos.1,
            right: window_pos.0 + size.0 as i32,
            bottom: window_pos.1 + size.1 as i32,
        };
        capture.update(BackdropCaptureRequest {
            share,
            origin: Point {
                x: window_pos.0,
                y: window_pos.1,
            },
            interest,
            read_value: self.backdrop.as_ref().map_or(0, SharedBackdrop::read_value),
            max_rate_hz: if animating { 60 } else { 30 },
        })
    }

    /// 在合成交换链上绘制一帧并呈现。
    fn draw_frame(
        &mut self,
        draw: impl FnOnce(&Canvas, Option<&LiquidBackdrop>) -> bool,
    ) -> Result<bool, String> {
        let target = &mut self.target;
        let present = target.current_surface()?;
        let canvas = present.canvas();
        // 恢复画布栈并清空整帧（含透明区域）
        canvas.restore_to_count(1);
        canvas.reset_matrix();
        canvas.save();
        let gpu_backdrop = match self.backdrop.as_mut() {
            Some(backdrop) => backdrop
                .images(&mut self.device.context)
                .map(|(_, blurred)| LiquidBackdrop {
                    image: blurred,
                    blur_scale: backdrop.frost_scale(),
                    origin: (0.0, 0.0),
                }),
            None => None,
        };
        let output = draw(canvas, gpu_backdrop.as_ref());
        present.canvas().restore_to_count(1);
        self.device.context.flush_surface_with_access(
            present,
            skia_safe::surfaces::BackendSurfaceAccess::Present,
            &skia_safe::gpu::FlushInfo::default(),
        );
        self.device.submit(skia_safe::gpu::SyncCpu::No)?;
        if let Some(backdrop) = self.backdrop.as_mut() {
            backdrop.finish_frame(self.device.queue())?;
        }
        let present_result = target.present();
        self.device.check_health()?;
        present_result?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::{BackdropCaptureFrame, D3DDevice};

    /// 冒烟测试：D3D12 设备、Skia D3D12 上下文、共享背板与 GPU frost 管线。
    /// 无 GPU 环境自动跳过；手动运行：cargo test gpu_device_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn gpu_device_smoke() {
        let Ok(mut device) = D3DDevice::new() else {
            eprintln!("跳过 GPU 冒烟测试：无可用 D3D12 设备");
            return;
        };
        assert!(device.check_health().is_ok(), "设备健康检查失败");
        let mut backdrop = device
            .create_backdrop(256, 256, 1)
            .expect("共享背板创建失败");
        let share = backdrop.share();
        assert_ne!(share.texture, 0, "共享纹理句柄为空");
        assert_ne!(share.write_fence, 0, "写围栏句柄为空");
        let queue = device.queue().clone();
        backdrop
            .set_frame(
                &queue,
                Some(BackdropCaptureFrame {
                    write_value: 0,
                    serial: 1,
                }),
                8.0,
            )
            .expect("背板帧设置失败");
        let images = backdrop.images(&mut device.context);
        assert!(images.is_some(), "GPU frost 管线未产出图像");
        drop(images);
        drop(backdrop);
        assert!(device.synchronize().is_ok(), "GPU 同步失败");
        eprintln!("GPU 冒烟测试通过：D3D12 设备、Skia 上下文与 frost 管线可用");
    }
}
