//! Windows.Graphics.Capture 屏幕捕获（移植自 WinIsland 的 platform-windows/capture.rs）。
//!
//! 在渲染适配器上创建 D3D11 设备，以自由线程帧池捕获窗口所在显示器；
//! 变化探针与脏区域用于节流，仅当窗口下方内容变化时才唤醒渲染线程。
//! 捕获端不管理窗口的 WDA 排除标志（由应用层统一设置）。

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use windows62::{
    Foundation::Metadata::ApiInformation,
    Foundation::TimeSpan,
    Graphics::Capture::{
        Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
        GraphicsCaptureAccessKind, GraphicsCaptureDirtyRegionMode, GraphicsCaptureItem,
        GraphicsCaptureSession,
    },
    Graphics::DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    Graphics::SizeInt32,
    Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus,
    Win32::{
        Foundation::{HANDLE, HMODULE, HWND, LUID},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1},
            Direct3D11::{
                D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice,
                ID3D11Device, ID3D11Device1, ID3D11Device5, ID3D11DeviceContext,
                ID3D11DeviceContext4, ID3D11Fence, ID3D11Multithread, ID3D11Texture2D,
            },
            Dxgi::{
                Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC},
                CreateDXGIFactory1, DXGI_ERROR_WAS_STILL_DRAWING, IDXGIAdapter, IDXGIDevice,
                IDXGIFactory4,
            },
            Gdi::{
                GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
            },
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
        },
    },
    core::{HSTRING, Interface},
};

use super::{BackdropCaptureFrame, BackdropCaptureRequest, BackdropShare, Rect};

const FRAME_BUFFERS: i32 = 2;
const PROBE_STEP: usize = 2;
const PROBE_ALIGN: u32 = 64;
const BORDER_UNKNOWN: u8 = 0;
const BORDER_PENDING: u8 = 1;
const BORDER_OPTIONAL: u8 = 2;
const BORDER_REQUIRED: u8 = 3;

static BORDER_ACCESS: AtomicU8 = AtomicU8::new(BORDER_UNKNOWN);

/// 锁定互斥量；锁中毒时仍继续使用内部数据（捕获线程崩溃不应拖垮渲染线程）。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Latest {
    frame: Option<Direct3D11CaptureFrame>,
    sequence: u64,
}

struct Shared {
    latest: Mutex<Latest>,
    interest: Mutex<Rect>,
    pending: AtomicBool,
    content: Mutex<SizeInt32>,
    device: AgileDevice,
    d3d: AgileContext,
    probe: Mutex<Probe>,
}

#[derive(Default)]
struct Probe {
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    rect: Rect,
    samples: Vec<u32>,
    scratch: Vec<u32>,
}

struct Session {
    monitor: HMONITOR,
    origin: (i32, i32),
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: i64,
    rate_hz: Cell<u32>,
    borderless: Cell<bool>,
}

struct Import {
    generation: u64,
    texture: ID3D11Texture2D,
    write_fence: ID3D11Fence,
    read_fence: ID3D11Fence,
    size: (u32, u32),
}

pub(crate) struct BackdropCapture {
    hwnd: HWND,
    device: ID3D11Device5,
    context: ID3D11DeviceContext4,
    shared: Arc<Shared>,
    session: RefCell<Option<Session>>,
    import: RefCell<Option<Import>>,
    copied: Cell<Option<(u64, u64, i32, i32)>>,
    verified: Cell<Rect>,
    write_value: AtomicU64,
    serial: Cell<u64>,
}

impl BackdropCapture {
    /// 创建捕获端并启动显示器捕获会话。
    pub(crate) fn new(hwnd_raw: isize, adapter_luid: u64) -> Result<Self, String> {
        if !GraphicsCaptureSession::IsSupported().unwrap_or(false) {
            return Err("Windows.Graphics.Capture 不受支持".to_string());
        }
        let hwnd = HWND(hwnd_raw as *mut _);
        let (device, context) = create_device(adapter_luid)?;
        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|error| format!("IDXGIDevice 不可用: {error}"))?;
        // SAFETY: DXGI 设备存活；WinRT 返回拥有的可检查对象包装。
        let winrt_device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .and_then(|device| device.cast())
            .map_err(|error| format!("WinRT Direct3D 设备创建失败: {error}"))?;
        let d3d = AgileContext(device.clone().into(), context.clone().into());
        let capture = Self {
            hwnd,
            device,
            context,
            shared: Arc::new(Shared {
                latest: Mutex::new(Latest::default()),
                interest: Mutex::new(Rect::default()),
                pending: AtomicBool::new(false),
                content: Mutex::new(SizeInt32::default()),
                device: AgileDevice(winrt_device),
                d3d,
                probe: Mutex::new(Probe::default()),
            }),
            session: RefCell::new(None),
            import: RefCell::new(None),
            copied: Cell::new(None),
            verified: Cell::new(Rect::default()),
            write_value: AtomicU64::new(0),
            serial: Cell::new(0),
        };
        capture.ensure_session(60)?;
        Ok(capture)
    }

    /// 处理一次背板更新请求：必要时重启会话，把最新帧拷贝进共享纹理。
    pub(crate) fn update(&self, request: BackdropCaptureRequest) -> Option<BackdropCaptureFrame> {
        if let Err(error) = self.ensure_session(request.max_rate_hz) {
            log::warn!("背板捕获会话重启失败: {error}");
            return None;
        }
        let origin = self.session.borrow().as_ref()?.origin;
        let interest = Rect {
            left: request.interest.left - origin.0,
            top: request.interest.top - origin.1,
            right: request.interest.right - origin.0,
            bottom: request.interest.bottom - origin.1,
        };
        *lock(&self.shared.interest) = interest;
        self.shared.pending.store(false, Ordering::Release);
        self.ensure_import(request.share)?;
        let (frame, sequence) = {
            let latest = lock(&self.shared.latest);
            (latest.frame.clone()?, latest.sequence)
        };
        let key = (
            request.share.generation,
            sequence,
            request.origin.x - origin.0,
            request.origin.y - origin.1,
        );
        let verified = intersection(self.verified.get(), interest);
        if self.copied.get() != Some(key) || verified != interest {
            self.copy(&frame, request, key)?;
            self.verified.set(interest);
        } else {
            self.verified.set(verified);
        }
        Some(BackdropCaptureFrame {
            write_value: self.write_value.load(Ordering::Acquire),
            serial: self.serial.get(),
        })
    }

    fn copy(
        &self,
        frame: &Direct3D11CaptureFrame,
        request: BackdropCaptureRequest,
        key: (u64, u64, i32, i32),
    ) -> Option<()> {
        let import = self.import.borrow();
        let import = import.as_ref()?;
        let source = frame_texture(frame).ok()?;
        let content = frame.ContentSize().ok()?;
        let (local_x, local_y) = (key.2, key.3);
        let left = local_x.max(0);
        let top = local_y.max(0);
        let right = (local_x + import.size.0 as i32).min(content.Width);
        let bottom = (local_y + import.size.1 as i32).min(content.Height);
        if right <= left || bottom <= top {
            return None;
        }
        let source_box = D3D11_BOX {
            left: left as u32,
            top: top as u32,
            front: 0,
            right: right as u32,
            bottom: bottom as u32,
            back: 1,
        };
        let value = self.write_value.load(Ordering::Acquire) + 1;
        // SAFETY: 各接口在该设备上存活；拷贝盒同时位于两个纹理内；
        // GPU 会先等待渲染器完成读取再覆写共享纹理。
        let result = unsafe {
            self.context
                .Wait(&import.read_fence, request.read_value)
                .map(|()| {
                    self.context.CopySubresourceRegion(
                        &import.texture,
                        0,
                        (left - local_x) as u32,
                        (top - local_y) as u32,
                        0,
                        &source,
                        0,
                        Some(&source_box),
                    );
                })
                .and_then(|()| self.context.Signal(&import.write_fence, value))
                .map(|()| self.context.Flush())
        };
        if let Err(error) = result {
            log::warn!("背板捕获拷贝失败: {error}");
            return None;
        }
        // SAFETY: 仅查询保留设备的移除状态。
        if let Err(error) = unsafe { self.device.GetDeviceRemovedReason() } {
            log::warn!("背板捕获设备已移除: {error}");
            return None;
        }
        self.write_value.store(value, Ordering::Release);
        self.serial.set(self.serial.get() + 1);
        self.copied.set(Some(key));
        Some(())
    }

    fn ensure_import(&self, share: BackdropShare) -> Option<()> {
        if self
            .import
            .borrow()
            .as_ref()
            .is_some_and(|import| import.generation == share.generation)
        {
            return Some(());
        }
        self.import.borrow_mut().take();
        let device1: ID3D11Device1 = self.device.cast().ok()?;
        // SAFETY: 该代次存续期间渲染器保持全部 NT 句柄开启；打开操作复制引用，
        // 因此本次导入可晚于句柄关闭而继续存活。
        let import = (|| unsafe {
            let texture: ID3D11Texture2D =
                device1.OpenSharedResource1(HANDLE(share.texture as *mut _))?;
            let mut write_fence: Option<ID3D11Fence> = None;
            self.device
                .OpenSharedFence(HANDLE(share.write_fence as *mut _), &mut write_fence)?;
            let mut read_fence: Option<ID3D11Fence> = None;
            self.device
                .OpenSharedFence(HANDLE(share.read_fence as *mut _), &mut read_fence)?;
            windows62::core::Result::Ok(Import {
                generation: share.generation,
                texture,
                write_fence: write_fence.ok_or_else(windows62::core::Error::empty)?,
                read_fence: read_fence.ok_or_else(windows62::core::Error::empty)?,
                size: (share.width, share.height),
            })
        })();
        match import {
            Ok(import) => {
                *self.import.borrow_mut() = Some(import);
                self.copied.set(None);
                Some(())
            }
            Err(error) => {
                log::warn!("背板共享资源导入失败: {error}");
                None
            }
        }
    }

    fn ensure_session(&self, rate_hz: u32) -> Result<(), String> {
        // SAFETY: hwnd 是存活的悬浮窗；该调用只读取其位置。
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        if let Some(session) = self.session.borrow().as_ref()
            && session.monitor == monitor
        {
            if session.rate_hz.get() != rate_hz {
                session.rate_hz.set(rate_hz);
                set_rate(&session.session, rate_hz);
            }
            if !session.borderless.get() && borderless_allowed() {
                session.borderless.set(true);
                hide_border(&session.session);
            }
            if border_access_required() {
                self.session.borrow_mut().take();
                return Err("无边框屏幕捕获权限不可用".to_string());
            }
            return Ok(());
        }
        self.session.borrow_mut().take();
        *lock(&self.shared.latest) = Latest::default();
        self.copied.set(None);
        let session = Session::start(monitor, &self.shared, rate_hz)?;
        *self.session.borrow_mut() = Some(session);
        Ok(())
    }
}

impl Session {
    fn start(monitor: HMONITOR, shared: &Arc<Shared>, rate_hz: u32) -> Result<Self, String> {
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: monitor 来自 MonitorFromWindow，info 是尺寸正确的输出结构。
        if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            return Err("显示器信息不可用".to_string());
        }
        let interop =
            windows62::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|error| format!("捕获互操作接口不可用: {error}"))?;
        // SAFETY: monitor 是存活的显示器句柄；WinRT 返回拥有的捕获项。
        let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(monitor) }
            .map_err(|error| format!("显示器捕获项创建失败: {error}"))?;
        let size = item
            .Size()
            .map_err(|error| format!("捕获尺寸不可用: {error}"))?;
        *lock(&shared.content) = size;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &shared.device.0,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            FRAME_BUFFERS,
            size,
        )
        .map_err(|error| format!("捕获帧池创建失败: {error}"))?;
        let weak = Arc::downgrade(shared);
        let token = pool
            .FrameArrived(&windows62::Foundation::TypedEventHandler::new(
                move |pool: windows62::core::Ref<'_, Direct3D11CaptureFramePool>, _| {
                    if let (Some(pool), Some(shared)) = (pool.as_ref(), weak.upgrade()) {
                        shared.receive(pool);
                    }
                    Ok(())
                },
            ))
            .map_err(|error| format!("捕获帧回调注册失败: {error}"))?;
        let session = pool
            .CreateCaptureSession(&item)
            .map_err(|error| format!("捕获会话创建失败: {error}"))?;
        let _ = session.SetIsCursorCaptureEnabled(false);
        let borderless = borderless_allowed();
        if border_access_required() {
            return Err("无边框屏幕捕获权限不可用".to_string());
        }
        if borderless {
            hide_border(&session);
        }
        let _ = session.SetDirtyRegionMode(GraphicsCaptureDirtyRegionMode::ReportOnly);
        set_rate(&session, rate_hz);
        session
            .StartCapture()
            .map_err(|error| format!("捕获启动失败: {error}"))?;
        Ok(Self {
            monitor,
            origin: (info.rcMonitor.left, info.rcMonitor.top),
            pool,
            session,
            token,
            rate_hz: Cell::new(rate_hz),
            borderless: Cell::new(borderless),
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

impl Shared {
    /// 采样窗口下方内容判断是否变化（探针），避免无谓重绘。
    fn probe_changed(&self, frame: &Direct3D11CaptureFrame, interest: Rect) -> bool {
        let content = *lock(&self.content);
        let rect = Rect {
            left: interest.left.max(0),
            top: interest.top.max(0),
            right: interest.right.min(content.Width),
            bottom: interest.bottom.min(content.Height),
        };
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return false;
        }
        let mut probe = lock(&self.probe);
        if let Err(error) = self.sample(&mut probe, frame, rect) {
            log::debug!("背板变化探针失败: {error}");
            return true;
        }
        let changed = probe.rect != rect || probe.samples != probe.scratch;
        let probe = &mut *probe;
        std::mem::swap(&mut probe.samples, &mut probe.scratch);
        probe.rect = rect;
        changed
    }

    fn sample(
        &self,
        probe: &mut Probe,
        frame: &Direct3D11CaptureFrame,
        rect: Rect,
    ) -> windows62::core::Result<()> {
        let AgileContext(device, context) = &self.d3d;
        let width = (rect.right - rect.left) as u32;
        let height = (rect.bottom - rect.top) as u32;
        if probe
            .staging
            .as_ref()
            .is_none_or(|(_, w, h)| *w < width || *h < height)
        {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width.div_ceil(PROBE_ALIGN) * PROBE_ALIGN,
                Height: height.div_ceil(PROBE_ALIGN) * PROBE_ALIGN,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                ..Default::default()
            };
            let mut texture = None;
            // SAFETY: 描述符已初始化，输出 Option 由本次调用初始化并拥有。
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut texture))? };
            probe.staging = Some((
                texture.ok_or_else(windows62::core::Error::empty)?,
                desc.Width,
                desc.Height,
            ));
        }
        let Some((staging, _, _)) = probe.staging.clone() else {
            return Err(windows62::core::Error::empty());
        };
        let source = frame_texture(frame)?;
        let source_box = D3D11_BOX {
            left: rect.left as u32,
            top: rect.top as u32,
            front: 0,
            right: rect.right as u32,
            bottom: rect.bottom as u32,
            back: 1,
        };
        probe.scratch.clear();
        // SAFETY: 拷贝盒位于帧内；暂存纹理不小于其尺寸；映射行只在拷贝宽度与
        // 高度范围内读取，且读取完成后立即解除映射。
        unsafe {
            context.CopySubresourceRegion(&staging, 0, 0, 0, 0, &source, 0, Some(&source_box));
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            let mut attempts = 0;
            loop {
                match context.Map(
                    &staging,
                    0,
                    D3D11_MAP_READ,
                    D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                    Some(&mut mapped),
                ) {
                    Ok(()) => break,
                    Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING && attempts < 50 => {
                        attempts += 1;
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    Err(error) => return Err(error),
                }
            }
            let base = mapped.pData as *const u8;
            for y in (0..height as usize).step_by(PROBE_STEP) {
                let row = base.add(y * mapped.RowPitch as usize).cast::<u32>();
                for x in (0..width as usize).step_by(PROBE_STEP) {
                    probe.scratch.push(row.add(x).read_unaligned());
                }
            }
            context.Unmap(&staging, 0);
        }
        Ok(())
    }

    /// 捕获回调（WGC 线程）：合并脏区域、更新最新帧，必要时唤醒渲染线程。
    fn receive(&self, pool: &Direct3D11CaptureFramePool) {
        let mut newest = None;
        let mut dirty: Option<Rect> = None;
        let mut whole = false;
        let mut unknown = false;
        while let Ok(frame) = pool.TryGetNextFrame() {
            match frame.DirtyRegions().and_then(|regions| {
                let mut rects = Vec::new();
                for index in 0..regions.Size()? {
                    rects.push(regions.GetAt(index)?);
                }
                Ok(rects)
            }) {
                Ok(rects) => {
                    for rect in rects {
                        let rect = Rect {
                            left: rect.X,
                            top: rect.Y,
                            right: rect.X + rect.Width,
                            bottom: rect.Y + rect.Height,
                        };
                        dirty = Some(dirty.map_or(rect, |acc| union(acc, rect)));
                    }
                }
                Err(_) => unknown = true,
            }
            newest = Some(frame);
        }
        let Some(frame) = newest else {
            return;
        };
        if let Ok(size) = frame.ContentSize() {
            let mut content = lock(&self.content);
            if size != *content {
                *content = size;
                let _ = pool.Recreate(
                    &self.device.0,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    FRAME_BUFFERS,
                    size,
                );
                whole = true;
            }
        }
        let interest = *lock(&self.interest);
        let relevant = whole
            || dirty.is_some_and(|dirty| {
                intersects(dirty, interest) && self.probe_changed(&frame, interest)
            })
            || (unknown && self.probe_changed(&frame, interest));
        {
            let mut latest = lock(&self.latest);
            latest.frame = Some(frame);
            if relevant {
                latest.sequence += 1;
            }
        }
        if relevant && !self.pending.swap(true, Ordering::AcqRel) {
            crate::gpu::wake();
        }
    }
}

impl Drop for BackdropCapture {
    fn drop(&mut self) {
        self.session.borrow_mut().take();
        *lock(&self.shared.latest) = Latest::default();
        self.import.borrow_mut().take();
    }
}

fn set_rate(session: &GraphicsCaptureSession, rate_hz: u32) {
    let interval = 10_000_000 / i64::from(rate_hz.clamp(1, 1000));
    let _ = session.SetMinUpdateInterval(TimeSpan { Duration: interval });
}

fn frame_texture(frame: &Direct3D11CaptureFrame) -> windows62::core::Result<ID3D11Texture2D> {
    let access: IDirect3DDxgiInterfaceAccess = frame.Surface()?.cast()?;
    // SAFETY: 表面包装的 D3D11 纹理由该帧拥有且存活。
    unsafe { access.GetInterface() }
}

fn create_device(adapter_luid: u64) -> Result<(ID3D11Device5, ID3D11DeviceContext4), String> {
    // SAFETY: DXGI 返回拥有的工厂与适配器，不保留调用方指针。
    let adapter: IDXGIAdapter = unsafe {
        CreateDXGIFactory1::<IDXGIFactory4>().and_then(|factory| {
            factory.EnumAdapterByLuid(LUID {
                LowPart: adapter_luid as u32,
                HighPart: (adapter_luid >> 32) as i32,
            })
        })
    }
    .map_err(|error| format!("捕获适配器查找失败: {error}"))?;
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: 适配器存活，两个输出 Option 均由本次调用初始化并拥有。
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }
    .map_err(|error| format!("捕获 D3D11 设备创建失败: {error}"))?;
    let device = device.ok_or("捕获 D3D11 设备缺失")?;
    let context = context.ok_or("捕获 D3D11 上下文缺失")?;
    if let Ok(multithread) = context.cast::<ID3D11Multithread>() {
        // SAFETY: 启用保护仅序列化该设备的即时上下文。
        let _ = unsafe { multithread.SetMultithreadProtected(true) };
    }
    Ok((
        device
            .cast()
            .map_err(|error| format!("ID3D11Device5 不可用: {error}"))?,
        context
            .cast()
            .map_err(|error| format!("ID3D11DeviceContext4 不可用: {error}"))?,
    ))
}

fn union(a: Rect, b: Rect) -> Rect {
    Rect {
        left: a.left.min(b.left),
        top: a.top.min(b.top),
        right: a.right.max(b.right),
        bottom: a.bottom.max(b.bottom),
    }
}

fn intersection(a: Rect, b: Rect) -> Rect {
    Rect {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right).max(a.left.max(b.left)),
        bottom: a.bottom.min(b.bottom).max(a.top.max(b.top)),
    }
}

fn intersects(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

struct AgileDevice(IDirect3DDevice);

struct AgileContext(ID3D11Device, ID3D11DeviceContext);

// SAFETY: WinRT Direct3D 设备是敏捷对象，且被包装的 D3D11 设备在共享给
// 捕获回调线程之前已启用多线程保护。
unsafe impl Send for AgileDevice {}
// SAFETY: 见 Send 实现；该设备仅通过线程安全的调用使用。
unsafe impl Sync for AgileDevice {}

// SAFETY: 设备已启用多线程保护，会在渲染线程与捕获回调线程之间序列化即时上下文。
unsafe impl Send for AgileContext {}
// SAFETY: 见 Send 实现。
unsafe impl Sync for AgileContext {}

fn borderless_allowed() -> bool {
    if !borderless_api_supported() {
        BORDER_ACCESS.store(BORDER_REQUIRED, Ordering::Release);
        return false;
    }
    if BORDER_ACCESS
        .compare_exchange(
            BORDER_UNKNOWN,
            BORDER_PENDING,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
    {
        let requested =
            GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
                .and_then(|operation| {
                    operation.when(|status| {
                        let allowed = status
                            .as_ref()
                            .is_ok_and(|status| *status == AppCapabilityAccessStatus::Allowed);
                        if !allowed {
                            log::info!("无边框屏幕捕获未获授权: {status:?}");
                        }
                        BORDER_ACCESS.store(
                            if allowed {
                                BORDER_OPTIONAL
                            } else {
                                BORDER_REQUIRED
                            },
                            Ordering::Release,
                        );
                        crate::gpu::wake();
                    })
                });
        if let Err(error) = requested {
            log::info!("无边框屏幕捕获不可用: {error}");
            BORDER_ACCESS.store(BORDER_REQUIRED, Ordering::Release);
        }
    }
    BORDER_ACCESS.load(Ordering::Acquire) == BORDER_OPTIONAL
}

fn border_access_required() -> bool {
    BORDER_ACCESS.load(Ordering::Acquire) == BORDER_REQUIRED
}

fn borderless_api_supported() -> bool {
    ApiInformation::IsPropertyPresent(
        &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &HSTRING::from("IsBorderRequired"),
    )
    .unwrap_or(false)
}

fn hide_border(session: &GraphicsCaptureSession) {
    if let Err(error) = session.SetIsBorderRequired(false) {
        log::warn!("屏幕捕获边框无法隐藏: {error}");
    }
}
