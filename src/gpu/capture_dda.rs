//! DXGI Desktop Duplication 背板捕获（液态玻璃-DXGI 后端）。
//!
//! 复制会话仅声明 B8G8R8A8_UNORM：SDR 桌面按原格式输出，HDR 桌面（FP16 scRGB）
//! 由 DXGI 在系统端转换为 BGRA8，下游共享纹理管线无需感知格式差异。
//! 轮询线程获取最新桌面帧并整体拷入内部纹理；主线程 update() 沿用与 WGC
//! 后端相同的共享纹理握手把 interest 区域拷进 D3D12 背板。
//! 桌面内容已由应用层的 WCA_EXCLUDED_FROM_DDA 排除岛屿窗口自身。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows62::Graphics::SizeInt32;
use windows62::Win32::Foundation::RECT;
use windows62::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, ID3D11Device5, ID3D11DeviceContext4,
    ID3D11Texture2D,
};
use windows62::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows62::Win32::Graphics::Dxgi::{
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO, IDXGIAdapter, IDXGIDevice, IDXGIOutput,
    IDXGIOutput1, IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource,
};
use windows62::Win32::Graphics::Gdi::HMONITOR;
use windows62::core::Interface;

use super::Rect;
use super::capture::{FrameSource, Shared, intersects, lock};

/// AcquireNextFrame 的阻塞超时：桌面无更新时的轮询间隔，也决定停止响应粒度。
const POLL_TIMEOUT_MS: u32 = 100;
/// 退避睡眠的分片粒度。
const STOP_CHECK_INTERVAL: Duration = Duration::from_millis(50);
/// 复制会话重建退避的起始与上限。
const REBUILD_BACKOFF_MIN: Duration = Duration::from_millis(100);
const REBUILD_BACKOFF_MAX: Duration = Duration::from_millis(2_000);

/// DDA 捕获会话：输出定位一次，复制会话由轮询线程在失效时自行重建。
pub(super) struct DdaSession {
    monitor: HMONITOR,
    origin: (i32, i32),
    rate_hz: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl DdaSession {
    /// 在窗口所在显示器对应的输出上启动 DDA 捕获轮询线程。
    pub(super) fn start(
        monitor: HMONITOR,
        shared: &Arc<Shared>,
        rate_hz: u32,
        device: &ID3D11Device5,
        context: &ID3D11DeviceContext4,
    ) -> Result<Self, String> {
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|error| format!("捕获 DXGI 设备不可用: {error}"))?;
        // SAFETY: 设备存活，返回其所属适配器。
        let adapter: IDXGIAdapter = unsafe { dxgi_device.GetAdapter() }
            .map_err(|error| format!("捕获适配器获取失败: {error}"))?;
        let output = find_output(&adapter, monitor)?;
        let duplication = create_duplication(&output, device)?;
        let origin = {
            // SAFETY: output 由适配器枚举拥有，desc 为输出结构。
            let desc = unsafe { output.GetDesc() }
                .map_err(|error| format!("输出描述获取失败: {error}"))?;
            (desc.DesktopCoordinates.left, desc.DesktopCoordinates.top)
        };
        // SAFETY: 仅读取复制会话描述，用于记录分辨率。
        let desc = unsafe { duplication.GetDesc() };
        log::info!(
            "DXGI 背板捕获已启动：{}x{} 格式 {:?}",
            desc.ModeDesc.Width,
            desc.ModeDesc.Height,
            desc.ModeDesc.Format
        );
        let rate_hz = Arc::new(AtomicU32::new(rate_hz));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            shared: shared.clone(),
            device: device.clone(),
            context: context.clone(),
            output,
            duplication: Some(duplication),
            internal: None,
            rate_hz: Arc::clone(&rate_hz),
            stop: Arc::clone(&stop),
            backoff: REBUILD_BACKOFF_MIN,
            last_wake: Instant::now(),
        };
        let handle = std::thread::Builder::new()
            .name("backdrop-dda".to_string())
            .spawn(move || worker.run())
            .map_err(|error| format!("DDA 捕获线程启动失败: {error}"))?;
        Ok(Self {
            monitor,
            origin,
            rate_hz,
            stop,
            worker: Some(handle),
        })
    }

    pub(super) fn monitor(&self) -> HMONITOR {
        self.monitor
    }

    pub(super) fn origin(&self) -> (i32, i32) {
        self.origin
    }

    pub(super) fn sync_rate(&self, rate_hz: u32) {
        self.rate_hz.store(rate_hz, Ordering::Release);
    }
}

impl Drop for DdaSession {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// 轮询线程状态：拥有复制会话与内部最新帧纹理。
struct Worker {
    shared: Arc<Shared>,
    device: ID3D11Device5,
    context: ID3D11DeviceContext4,
    output: IDXGIOutput,
    duplication: Option<IDXGIOutputDuplication>,
    internal: Option<(ID3D11Texture2D, u32, u32)>,
    rate_hz: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    backoff: Duration,
    last_wake: Instant,
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            let Some(duplication) = self.duplication.clone() else {
                self.rebuild();
                continue;
            };
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            // SAFETY: 复制会话存活；输出 Option 由本次调用初始化并拥有。
            match unsafe {
                duplication.AcquireNextFrame(POLL_TIMEOUT_MS, &mut frame_info, &mut resource)
            } {
                // 桌面无更新，继续等待
                Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => continue,
                // 模式切换/安全桌面/并发上限等：丢弃会话，退避重建
                Err(error) => {
                    log::info!("DXGI 背板捕获会话失效，稍后重建: {error}");
                    self.duplication = None;
                    continue;
                }
                Ok(()) => {}
            }
            // 脏区必须在 ReleaseFrame 之前读取
            let dirty_rects = read_dirty_rects(&duplication, &frame_info);
            let copied = resource
                .and_then(|resource| resource.cast::<ID3D11Texture2D>().ok())
                .is_some_and(|texture| self.copy_frame(&texture));
            // SAFETY: 帧已处理完毕，释放后表面交还 DWM；拷贝命令已提交到上下文。
            let _ = unsafe { duplication.ReleaseFrame() };
            if copied {
                self.publish_frame(&dirty_rects);
            }
        }
    }

    /// 复制会话缺失时按退避重建；等待期间分片响应停止标志。
    fn rebuild(&mut self) {
        if !sleep_interruptible(self.backoff, &self.stop) {
            return;
        }
        match create_duplication(&self.output, &self.device) {
            Ok(duplication) => {
                self.duplication = Some(duplication);
                self.backoff = REBUILD_BACKOFF_MIN;
            }
            Err(error) => {
                log::info!("DXGI 背板复制会话重建失败: {error}");
                self.backoff = (self.backoff * 2).min(REBUILD_BACKOFF_MAX);
            }
        }
    }

    /// 把新桌面帧整体拷入内部纹理；必要时按源描述重建并更新共享帧源。
    fn copy_frame(&mut self, texture: &ID3D11Texture2D) -> bool {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc 为输出结构，由本次调用初始化。
        unsafe { texture.GetDesc(&mut desc) };
        // 内部纹理只作为拷贝中转，剥离共享与渲染目标标志以降低创建失败面
        desc.MiscFlags = 0;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        desc.CPUAccessFlags = 0;
        let (width, height) = (desc.Width, desc.Height);
        if self
            .internal
            .as_ref()
            .is_none_or(|(_, w, h)| *w != width || *h != height)
        {
            let mut new_texture: Option<ID3D11Texture2D> = None;
            // SAFETY: 描述符已初始化，输出 Option 由本次调用初始化并拥有。
            if let Err(error) = unsafe {
                self.device
                    .CreateTexture2D(&desc, None, Some(&mut new_texture))
            } {
                log::warn!("DDA 内部帧纹理创建失败: {error}");
                return false;
            }
            let Some(new_texture) = new_texture else {
                return false;
            };
            self.internal = Some((new_texture.clone(), width, height));
            {
                let mut latest = lock(&self.shared.latest);
                latest.source = Some(FrameSource::Dda {
                    texture: new_texture,
                    size: (width, height),
                });
            }
            *lock(&self.shared.content) = SizeInt32 {
                Width: width as i32,
                Height: height as i32,
            };
        }
        let Some((internal, _, _)) = self.internal.as_ref() else {
            return false;
        };
        // SAFETY: 两纹理同尺寸同格式；设备上下文已启用多线程保护。
        unsafe { self.context.CopyResource(internal, texture) };
        // SAFETY: 仅提交已排队的拷贝命令，使其在 ReleaseFrame 前进入执行队列。
        unsafe { self.context.Flush() };
        true
    }

    /// 按脏区相关性与速率节流决定是否唤醒渲染线程。
    fn publish_frame(&mut self, dirty_rects: &[RECT]) {
        if !dirty_rects.is_empty() {
            let interest = *lock(&self.shared.interest);
            let relevant = dirty_rects.iter().any(|rect| {
                intersects(
                    Rect {
                        left: rect.left,
                        top: rect.top,
                        right: rect.right,
                        bottom: rect.bottom,
                    },
                    interest,
                )
            });
            if !relevant {
                return;
            }
        }
        let rate_hz = self.rate_hz.load(Ordering::Acquire).max(1);
        let min_interval = Duration::from_millis(1000 / u64::from(rate_hz));
        let now = Instant::now();
        if now.duration_since(self.last_wake) < min_interval {
            return;
        }
        self.last_wake = now;
        lock(&self.shared.latest).sequence += 1;
        if !self.shared.pending.swap(true, Ordering::AcqRel) {
            crate::gpu::wake();
        }
    }
}

/// 读取帧脏区；必须在 ReleaseFrame 之前调用，失败或无脏区时返回空。
fn read_dirty_rects(
    duplication: &IDXGIOutputDuplication,
    frame_info: &DXGI_OUTDUPL_FRAME_INFO,
) -> Vec<RECT> {
    if frame_info.TotalMetadataBufferSize == 0 {
        return Vec::new();
    }
    // TotalMetadataBufferSize 是移动区与脏区元数据的总上限，按 RECT 大小换算
    // 的容量必然足够
    let capacity = frame_info.TotalMetadataBufferSize as usize / std::mem::size_of::<RECT>() + 1;
    let mut rects = vec![RECT::default(); capacity];
    let mut required: u32 = 0;
    // SAFETY: 缓冲区不小于元数据总上限；所需字节数写入 required。
    if let Err(error) = unsafe {
        duplication.GetFrameDirtyRects(
            (rects.len() * std::mem::size_of::<RECT>()) as u32,
            rects.as_mut_ptr(),
            &mut required,
        )
    } {
        log::debug!("读取帧脏区失败: {error}");
        return Vec::new();
    }
    let count = (required as usize / std::mem::size_of::<RECT>()).min(rects.len());
    rects.truncate(count);
    rects
}

/// 在适配器上查找包含指定显示器的输出。
fn find_output(adapter: &IDXGIAdapter, monitor: HMONITOR) -> Result<IDXGIOutput, String> {
    for index in 0_u32.. {
        // SAFETY: adapter 存活；枚举越界返回 Err 即结束。
        let output = match unsafe { adapter.EnumOutputs(index) } {
            Ok(output) => output,
            Err(_) => break,
        };
        // SAFETY: output 由本次枚举拥有，desc 为输出结构。
        if let Ok(desc) = unsafe { output.GetDesc() }
            && desc.Monitor == monitor
        {
            return Ok(output);
        }
    }
    Err("显示器对应的 DXGI 输出不在捕获适配器上".to_string())
}

/// 在输出上创建桌面复制会话；优先 DuplicateOutput1（仅声明 BGRA8，HDR 桌面
/// 由 DXGI 端转换为 SDR），失败时回退旧版 DuplicateOutput。
fn create_duplication(
    output: &IDXGIOutput,
    device: &ID3D11Device5,
) -> Result<IDXGIOutputDuplication, String> {
    if let Ok(output5) = output.cast::<IDXGIOutput5>() {
        // SAFETY: device 与 output 属于同一适配器且均存活。
        match unsafe { output5.DuplicateOutput1(device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) } {
            Ok(duplication) => return verify_duplication(duplication),
            Err(error) => {
                log::warn!("DuplicateOutput1 不可用，回退旧版 DuplicateOutput: {error}");
            }
        }
    }
    let output1: IDXGIOutput1 = output
        .cast()
        .map_err(|error| format!("IDXGIOutput1 不可用: {error}"))?;
    // SAFETY: device 与 output 属于同一适配器且均存活。
    let duplication = unsafe { output1.DuplicateOutput(device) }
        .map_err(|error| format!("桌面复制会话创建失败: {error}"))?;
    verify_duplication(duplication)
}

/// 校验复制会话输出格式为 BGRA8（下游共享纹理管线的格式约定）。
fn verify_duplication(
    duplication: IDXGIOutputDuplication,
) -> Result<IDXGIOutputDuplication, String> {
    // SAFETY: 仅读取复制会话描述。
    let desc = unsafe { duplication.GetDesc() };
    if desc.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        return Err(format!(
            "桌面复制输出格式不受支持: {:?}",
            desc.ModeDesc.Format
        ));
    }
    Ok(duplication)
}

/// 分片睡眠，期间响应停止标志；返回 false 表示线程应退出。
fn sleep_interruptible(total: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + total;
    loop {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep(STOP_CHECK_INTERVAL.min(deadline - now));
    }
}
