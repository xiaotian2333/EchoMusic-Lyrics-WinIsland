//! DXGI Desktop Duplication 背板捕获（液态玻璃-DXGI 后端）。
//!
//! 复制会话仅声明 B8G8R8A8_UNORM：SDR 桌面按原格式输出，HDR 桌面（FP16 scRGB）
//! 由 DXGI 在系统端转换为 BGRA8，下游共享纹理管线无需感知格式差异。
//! 轮询线程按脏区相关性过滤后，只把 interest 交叠盒（窗口所在区域）拷入内部
//! 纹理；主线程 update() 沿用与 WGC 后端相同的共享纹理握手把该区域拷进 D3D12
//! 背板。interest 之外的桌面更新不产生拷贝与唤醒，避免全桌面大拷贝在桌面刷新
//! 率下与主线程渲染争抢 GPU。
//! 会话（重）启动时尽力重新应用 WCA_EXCLUDED_FROM_DDA；部分系统会静默忽略
//! 该标志，此时捕获包含岛屿自身渲染内容，由液态玻璃的重度模糊掩盖，
//! 不做任何后端替换。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows62::Graphics::SizeInt32;
use windows62::Win32::Foundation::RECT;
use windows62::Win32::Graphics::Direct3D11::{
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, ID3D11Device5,
    ID3D11DeviceContext4, ID3D11Texture2D,
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
        hwnd_raw: isize,
        monitor: HMONITOR,
        shared: &Arc<Shared>,
        rate_hz: u32,
        device: &ID3D11Device5,
        context: &ID3D11DeviceContext4,
    ) -> Result<Self, String> {
        // 会话（重）启动时重新应用排除标志：应用层只在样式切换时设置一次，DWM
        // 可能因窗口属性变动丢失；尽力而为，设置结果不影响会话启动
        if !super::set_excluded_from_dda(hwnd_raw, true) {
            log::warn!("WCA_EXCLUDED_FROM_DDA 设置失败，DDA 捕获将包含岛屿自身");
        }
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
            hwnd_raw,
            origin,
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
            deferred_wake: None,
            format_warned: false,
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
    hwnd_raw: isize,
    /// 输出左上角的桌面坐标，全帧兜底拷贝时作为内部纹理原点发布。
    origin: (i32, i32),
    shared: Arc<Shared>,
    device: ID3D11Device5,
    context: ID3D11DeviceContext4,
    output: IDXGIOutput,
    duplication: Option<IDXGIOutputDuplication>,
    /// (纹理, 宽, 高, 纹理左上角桌面坐标)
    internal: Option<(ID3D11Texture2D, u32, u32, (i32, i32))>,
    rate_hz: Arc<AtomicU32>,
    stop: Arc<AtomicBool>,
    backoff: Duration,
    last_wake: Instant,
    /// 节流窗内攒下的补发唤醒时刻；窗口结束时补发一次，突发末帧不被吞。
    deferred_wake: Option<Instant>,
    /// 帧格式告警是否已输出（只告警一次，避免每帧刷屏）。
    format_warned: bool,
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            self.fire_deferred_wake();
            let Some(duplication) = self.duplication.clone() else {
                self.rebuild();
                continue;
            };
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut resource: Option<IDXGIResource> = None;
            // SAFETY: 复制会话存活；输出 Option 由本次调用初始化并拥有。
            // 有补发唤醒待触发时把超时截止到补发时刻，保证空闲时准时醒来补发。
            match unsafe {
                duplication.AcquireNextFrame(self.wake_wait_ms(), &mut frame_info, &mut resource)
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
            // 脏区必须在 ReleaseFrame 之前读取；相关性在拷贝之前判断，
            // 不相关的桌面更新不再产生全帧拷贝。interest 为空表示渲染端尚未
            // 确认本会话（其写入依赖先看到帧源），此时必须放行走全帧兜底，
            // 否则「等 interest 才产 source、等 source 才写 interest」互相死锁
            let dirty_rects = read_dirty_rects(&duplication, &frame_info);
            let interest = *lock(&self.shared.interest);
            let interest_ready = interest.right > interest.left && interest.bottom > interest.top;
            let relevant = !interest_ready
                || dirty_rects.is_empty()
                || dirty_rects.iter().any(|rect| {
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
            let copied = relevant
                && resource
                    .and_then(|resource| resource.cast::<ID3D11Texture2D>().ok())
                    .is_some_and(|texture| self.copy_frame(&texture, &interest));
            // SAFETY: 帧已处理完毕，释放后表面交还 DWM；拷贝命令已提交到上下文。
            let _ = unsafe { duplication.ReleaseFrame() };
            if copied {
                self.publish_frame();
            }
        }
    }

    /// 复制会话缺失时按退避重建；等待期间分片响应停止标志。
    fn rebuild(&mut self) {
        if !sleep_interruptible(self.backoff, &self.stop) {
            return;
        }
        if !super::set_excluded_from_dda(self.hwnd_raw, true) {
            log::warn!("WCA_EXCLUDED_FROM_DDA 重新应用失败，DDA 捕获将包含岛屿自身");
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

    /// 把新桌面帧中 interest 交叠盒拷入内部纹理；必要时按盒尺寸重建并更新共享帧源。
    ///
    /// interest 为空（渲染端尚未写入）时回退全帧拷贝，保持启动期行为。
    /// 只拷贝窗口所在区域把每帧 GPU 拷贝量从全桌面（4K 约 31.6MB）降到 interest
    /// 盒（约 0.1MB），避免以桌面刷新率全速执行的大拷贝与主线程渲染争抢 GPU。
    fn copy_frame(&mut self, texture: &ID3D11Texture2D, interest: &Rect) -> bool {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: desc 为输出结构，由本次调用初始化。
        unsafe { texture.GetDesc(&mut desc) };
        // 帧纹理格式运行时检查：桌面为 HDR（FP16）时复制描述符仍报 FP16，但
        // 两条复制路径交付的帧都应已转换为 BGRA8；异常格式直接拒绝本帧
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            if !self.format_warned {
                self.format_warned = true;
                log::warn!("DDA 帧格式异常: {:?}，跳过拷贝", desc.Format);
            }
            return false;
        }
        // 内部纹理只作为拷贝中转，剥离共享与渲染目标标志以降低创建失败面
        desc.MiscFlags = 0;
        desc.BindFlags = D3D11_BIND_SHADER_RESOURCE.0 as u32;
        desc.CPUAccessFlags = 0;
        // interest 盒裁剪到帧范围；完全落在帧外时本帧无事可做
        let interest_box = if interest.right > interest.left && interest.bottom > interest.top {
            let left = interest.left.clamp(0, desc.Width as i32);
            let top = interest.top.clamp(0, desc.Height as i32);
            let right = interest.right.clamp(0, desc.Width as i32);
            let bottom = interest.bottom.clamp(0, desc.Height as i32);
            if right <= left || bottom <= top {
                return false;
            }
            Some(D3D11_BOX {
                left: left as u32,
                top: top as u32,
                front: 0,
                right: right as u32,
                bottom: bottom as u32,
                back: 1,
            })
        } else {
            None
        };
        let (width, height) = match &interest_box {
            Some(box_) => (box_.right - box_.left, box_.bottom - box_.top),
            None => (desc.Width, desc.Height),
        };
        // 内部纹理左上角的桌面坐标：全帧兜底为输出原点，裁剪盒为其偏移后的原点
        let texture_origin = match &interest_box {
            Some(box_) => (
                self.origin.0 + box_.left as i32,
                self.origin.1 + box_.top as i32,
            ),
            None => self.origin,
        };
        let size_matches = self
            .internal
            .as_ref()
            .is_some_and(|(_, w, h, _)| *w == width && *h == height);
        if size_matches {
            // 尺寸不变但 interest 盒移动（窗口位移）时，仅更新发布帧的原点基准
            if let Some((texture, _, _, stored_origin)) = self.internal.as_mut()
                && *stored_origin != texture_origin
            {
                *stored_origin = texture_origin;
                let texture = texture.clone();
                let mut latest = lock(&self.shared.latest);
                latest.source = Some(FrameSource::Dda {
                    texture,
                    size: (width, height),
                    origin: texture_origin,
                });
            }
        } else {
            let mut texture_desc = desc;
            texture_desc.Width = width;
            texture_desc.Height = height;
            let mut new_texture: Option<ID3D11Texture2D> = None;
            // SAFETY: 描述符已初始化，输出 Option 由本次调用初始化并拥有。
            if let Err(error) = unsafe {
                self.device
                    .CreateTexture2D(&texture_desc, None, Some(&mut new_texture))
            } {
                log::warn!("DDA 内部帧纹理创建失败: {error}");
                return false;
            }
            let Some(new_texture) = new_texture else {
                return false;
            };
            self.internal = Some((new_texture.clone(), width, height, texture_origin));
            {
                let mut latest = lock(&self.shared.latest);
                latest.source = Some(FrameSource::Dda {
                    texture: new_texture,
                    size: (width, height),
                    origin: texture_origin,
                });
            }
            *lock(&self.shared.content) = SizeInt32 {
                Width: width as i32,
                Height: height as i32,
            };
        }
        let Some((internal, _, _, _)) = self.internal.as_ref() else {
            return false;
        };
        // SAFETY: 拷贝盒同时位于两个纹理内；设备上下文已启用多线程保护。
        unsafe {
            match &interest_box {
                Some(box_) => {
                    self.context
                        .CopySubresourceRegion(internal, 0, 0, 0, 0, texture, 0, Some(box_))
                }
                None => self.context.CopyResource(internal, texture),
            }
        }
        // SAFETY: 仅提交已排队的拷贝命令，使其在 ReleaseFrame 前进入执行队列。
        unsafe { self.context.Flush() };
        true
    }

    /// 递增共享帧序号并按速率节流唤醒渲染线程。
    ///
    /// 数据无条件拷进内部纹理并递增序号，被节流的只有唤醒：窗内的唤醒推迟到
    /// 窗口结束补发而不是丢弃，突发末帧不再滞留在内部纹理里——空闲态下最后一
    /// 次内容变化也能到达渲染端。补发只影响唤醒时机，后续帧的拷贝与发布不受影响，
    /// 唤醒节奏仍收敛在速率上限。
    fn publish_frame(&mut self) {
        lock(&self.shared.latest).sequence += 1;
        let rate_hz = self.rate_hz.load(Ordering::Acquire).max(1);
        let min_interval = Duration::from_millis(1000 / u64::from(rate_hz));
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_wake);
        if elapsed < min_interval {
            // 只保留最早的补发时刻；窗口结束前统一醒来发一次
            if self.deferred_wake.is_none() {
                self.deferred_wake = Some(now + (min_interval - elapsed));
            }
            return;
        }
        self.fire_wake();
    }

    /// 唤醒渲染线程并记录唤醒时刻（节流窗口基准），常规路径与补发路径共用。
    fn fire_wake(&mut self) {
        self.last_wake = Instant::now();
        if !self.shared.pending.swap(true, Ordering::AcqRel) {
            crate::gpu::wake();
        }
    }

    /// 补发节流窗内攒下的唤醒；突发末帧的唤醒不再被节流丢弃。
    fn fire_deferred_wake(&mut self) {
        let Some(deadline) = self.deferred_wake else {
            return;
        };
        if Instant::now() < deadline {
            return;
        }
        self.deferred_wake = None;
        self.fire_wake();
    }

    /// AcquireNextFrame 的等待时长：无补发时用常规轮询间隔；有补发时截止到
    /// 补发时刻（至少 1ms），空闲桌面下也能准时醒来补发。
    fn wake_wait_ms(&self) -> u32 {
        self.deferred_wake.map_or(POLL_TIMEOUT_MS, |deadline| {
            let remaining = deadline.saturating_duration_since(Instant::now());
            (remaining.as_millis() as u32 + 1).min(POLL_TIMEOUT_MS)
        })
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

/// 在输出上创建桌面复制会话；优先 DuplicateOutput1（仅声明 BGRA8），失败时
/// 回退旧版 DuplicateOutput（文档保证恒定转换为 32 位 BGRA）。
///
/// 注意：复制描述符的 ModeDesc.Format 始终是桌面原生格式（HDR 桌面下为 FP16），
/// 而格式转换发生在 AcquireNextFrame 交付的帧上，因此这里不做描述符级格式
/// 校验（否则 HDR 桌面下会话永远起不来），改由 copy_frame 对实际收到的帧
/// 纹理格式做运行时检查。
fn create_duplication(
    output: &IDXGIOutput,
    device: &ID3D11Device5,
) -> Result<IDXGIOutputDuplication, String> {
    if let Ok(output5) = output.cast::<IDXGIOutput5>() {
        // SAFETY: device 与 output 属于同一适配器且均存活。
        match unsafe { output5.DuplicateOutput1(device, 0, &[DXGI_FORMAT_B8G8R8A8_UNORM]) } {
            Ok(duplication) => return Ok(duplication),
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
