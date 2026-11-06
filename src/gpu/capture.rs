//! 背板屏幕捕获（液态玻璃 GPU 渲染管线），两种后端共用共享纹理与围栏握手：
//!
//! - WGC（液态玻璃-WGC）：Windows.Graphics.Capture 自由线程帧池捕获窗口所在显示器；
//! - DDA（液态玻璃-DXGI）：DXGI Desktop Duplication 轮询捕获，见 capture_dda.rs。
//!
//! 在渲染适配器上创建 D3D11 设备；变化探针与脏区域用于节流，仅当窗口下方
//! 内容变化时才唤醒渲染线程。捕获端不管理窗口的捕获排除标志
//! （WDA / WCA_EXCLUDED_FROM_DDA 由应用层按样式统一设置），但兼容模式的
//! 放大镜链路全部失效时会在运行时自行设置 WDA 以回退整屏捕获，并在 Drop
//! 时清除这一自设标志（self_excluded）。

use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

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
        Foundation::{HANDLE, HMODULE, HWND, LUID, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1},
            Direct3D11::{
                D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE,
                D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
                D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11Device5, ID3D11DeviceContext,
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

use super::CaptureBackend;
use super::capture_dda::DdaSession;
use super::capture_magnifier::{FrameSink, Magnifier, PixelFrame};
use super::{BackdropCaptureFrame, BackdropCaptureRequest, BackdropShare, Rect};

const FRAME_BUFFERS: i32 = 2;
const PROBE_STEP: usize = 2;
const PROBE_ALIGN: u32 = 64;
/// 像素回调子模式的帧率上限（CPU 读回成本高）。
const PIXEL_RATE_LIMIT: u32 = 60;
const BORDER_UNKNOWN: u8 = 0;
const BORDER_PENDING: u8 = 1;
const BORDER_OPTIONAL: u8 = 2;
const BORDER_REQUIRED: u8 = 3;

static BORDER_ACCESS: AtomicU8 = AtomicU8::new(BORDER_UNKNOWN);

/// 锁定互斥量；锁中毒时仍继续使用内部数据（捕获线程崩溃不应拖垮渲染线程）。
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
pub(super) struct Latest {
    pub(super) source: Option<FrameSource>,
    pub(super) sequence: u64,
}

pub(super) struct Shared {
    pub(super) latest: Mutex<Latest>,
    pub(super) interest: Mutex<Rect>,
    pub(super) pending: AtomicBool,
    pub(super) content: Mutex<SizeInt32>,
    /// WGC 帧池专用的 WinRT Direct3D 设备；DDA 后端不使用。
    device: AgileDevice,
    /// WGC 变化探针专用的设备与上下文包装；DDA 后端不使用。
    d3d: AgileContext,
    probe: Mutex<Probe>,
}

/// 最新背板帧的来源：WGC 帧池帧、DDA interest 裁剪纹理或放大镜像素回调帧。
#[derive(Clone)]
pub(super) enum FrameSource {
    Wgc(Direct3D11CaptureFrame),
    Dda {
        texture: ID3D11Texture2D,
        size: (u32, u32),
        /// 纹理左上角的桌面坐标：启动期全帧兜底时为输出原点，裁剪期为其 interest 盒原点
        origin: (i32, i32),
    },
    Magnifier(Arc<PixelFrame>),
}

impl FrameSource {
    /// 帧纹理；WGC 帧每次重新提取表面，DDA 克隆内部纹理引用。
    pub(super) fn texture(&self) -> windows62::core::Result<ID3D11Texture2D> {
        match self {
            Self::Wgc(frame) => frame_texture(frame),
            Self::Dda { texture, .. } => Ok(texture.clone()),
            // 放大镜像素帧由 copy() 经 upload_pixels 上传，不走纹理接口
            Self::Magnifier(_) => Err(windows62::core::Error::empty()),
        }
    }

    /// 帧内容尺寸（桌面像素）。
    pub(super) fn content_size(&self) -> windows62::core::Result<SizeInt32> {
        match self {
            Self::Wgc(frame) => frame.ContentSize(),
            Self::Dda { size, .. } => Ok(SizeInt32 {
                Width: size.0 as i32,
                Height: size.1 as i32,
            }),
            Self::Magnifier(_) => Err(windows62::core::Error::empty()),
        }
    }
}

#[derive(Default)]
struct Probe {
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    rect: Rect,
    samples: Vec<u32>,
    scratch: Vec<u32>,
}

/// 捕获会话后端；对上提供 monitor/origin/sync_rate/refresh 的统一视图。
enum Session {
    Wgc(WgcSession),
    Dda(DdaSession),
    Magnifier(MagnifierSession),
}

/// 放大镜捕获会话：帧经放大镜像素回调直接送达（无 WGC 会话）。
struct MagnifierSession {
    monitor: HMONITOR,
    origin: Cell<(i32, i32)>,
    rate_hz: Cell<u32>,
    magnifier: Magnifier,
}

impl Session {
    fn monitor(&self) -> HMONITOR {
        match self {
            Self::Wgc(session) => session.monitor,
            Self::Dda(session) => session.monitor(),
            Self::Magnifier(session) => session.monitor,
        }
    }

    fn origin(&self) -> (i32, i32) {
        match self {
            Self::Wgc(session) => session.origin,
            Self::Dda(session) => session.origin(),
            Self::Magnifier(session) => session.origin.get(),
        }
    }

    fn sync_rate(&self, rate_hz: u32) {
        match self {
            Self::Wgc(session) => {
                if session.rate_hz.get() != rate_hz {
                    session.rate_hz.set(rate_hz);
                    set_rate(&session.session, rate_hz);
                }
            }
            Self::Dda(session) => session.sync_rate(rate_hz),
            Self::Magnifier(session) => {
                if session.rate_hz.get() != rate_hz {
                    session.rate_hz.set(rate_hz);
                    session.magnifier.set_rate(rate_hz);
                }
            }
        }
    }

    /// 周期性状态复查；WGC 复查无边框捕获授权，DDA 由轮询线程自行恢复。
    fn refresh(&self) -> Result<(), String> {
        match self {
            Self::Wgc(session) => {
                if !session.borderless.get() && borderless_allowed() {
                    session.borderless.set(true);
                    hide_border(&session.session);
                }
                if border_access_required() {
                    return Err("无边框屏幕捕获权限不可用".to_string());
                }
                Ok(())
            }
            Self::Dda(_) => Ok(()),
            Self::Magnifier(_) => Ok(()),
        }
    }

    fn start(
        backend: CaptureBackend,
        hwnd_raw: isize,
        monitor: HMONITOR,
        shared: &Arc<Shared>,
        rate_hz: u32,
        device: &ID3D11Device5,
        context: &ID3D11DeviceContext4,
    ) -> Result<Self, String> {
        match backend {
            CaptureBackend::Wgc => Ok(Self::Wgc(WgcSession::start(monitor, shared, rate_hz)?)),
            CaptureBackend::Dda => Ok(Self::Dda(DdaSession::start(
                hwnd_raw, monitor, shared, rate_hz, device, context,
            )?)),
        }
    }
}

struct WgcSession {
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
    backend: CaptureBackend,
    device: ID3D11Device5,
    context: ID3D11DeviceContext4,
    shared: Arc<Shared>,
    session: RefCell<Option<Session>>,
    import: RefCell<Option<Import>>,
    copied: Cell<Option<(u64, u64, i32, i32)>>,
    verified: Cell<Rect>,
    write_value: AtomicU64,
    serial: Cell<u64>,
    /// 兼容模式：优先经放大镜采集背景，岛窗口对捕获可见。
    magnifier_mode: Cell<bool>,
    /// 放大镜像素帧的暂存上传纹理。
    upload: RefCell<Option<(ID3D11Texture2D, u32, u32)>>,
    /// 捕获端在放大镜链路全部失效回退整屏时自设的 WDA 标记；Drop 时据此清除。
    self_excluded: Cell<bool>,
}

impl BackdropCapture {
    /// 创建捕获端并按后端启动显示器捕获会话；`wgc_compat` 仅对 WGC 后端生效。
    pub(crate) fn new(
        hwnd_raw: isize,
        adapter_luid: u64,
        backend: CaptureBackend,
        wgc_compat: bool,
    ) -> Result<Self, String> {
        if backend == CaptureBackend::Wgc && !GraphicsCaptureSession::IsSupported().unwrap_or(false)
        {
            return Err("Windows.Graphics.Capture 不受支持".to_string());
        }
        let hwnd = HWND(hwnd_raw as *mut _);
        let (device, context) = create_device(adapter_luid)?;
        let dxgi: IDXGIDevice = device
            .cast()
            .map_err(|error| format!("IDXGIDevice 不可用: {error}"))?;
        // SAFETY: DXGI 设备存活；WinRT 返回拥有的可检查对象包装。
        // （WGC 帧池专用，DDA 后端不使用，创建成本可忽略。）
        let winrt_device: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
            .and_then(|device| device.cast())
            .map_err(|error| format!("WinRT Direct3D 设备创建失败: {error}"))?;
        let d3d = AgileContext(device.clone().into(), context.clone().into());
        let capture = Self {
            hwnd,
            backend,
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
            magnifier_mode: Cell::new(backend == CaptureBackend::Wgc && wgc_compat),
            upload: RefCell::new(None),
            self_excluded: Cell::new(false),
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
        // 像素回调子模式：放大镜源取 interest 与所在显示器的交集（屏幕外无内容）
        if let Some(Session::Magnifier(session)) = self.session.borrow().as_ref()
            && let Some(source) = monitor_clamped(self.hwnd, request.interest)
        {
            session.magnifier.set_source(source);
        }
        let (source, sequence) = {
            let latest = lock(&self.shared.latest);
            (latest.source.clone()?, latest.sequence)
        };
        // 放大镜像素帧的坐标基准跟随其源矩形；其余来源以会话原点为准
        let origin = match &source {
            FrameSource::Magnifier(frame) => frame.origin,
            _ => self.session.borrow().as_ref()?.origin(),
        };
        let interest = Rect {
            left: request.interest.left - origin.0,
            top: request.interest.top - origin.1,
            right: request.interest.right - origin.0,
            bottom: request.interest.bottom - origin.1,
        };
        *lock(&self.shared.interest) = interest;
        self.shared.pending.store(false, Ordering::Release);
        self.ensure_import(request.share)?;
        // DDA 内部纹理的坐标基准随帧携带（启动期全帧兜底为输出原点，裁剪期为其
        // interest 盒原点）；其余来源的源纹理从会话原点（输出左上角）起算
        let (base_x, base_y) = match &source {
            FrameSource::Dda { origin, .. } => *origin,
            _ => (origin.0, origin.1),
        };
        let key = (
            request.share.generation,
            sequence,
            request.origin.x - base_x,
            request.origin.y - base_y,
        );
        let verified = intersection(self.verified.get(), interest);
        if self.copied.get() != Some(key) || verified != interest {
            self.copy(&source, request, key)?;
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
        frame_source: &FrameSource,
        request: BackdropCaptureRequest,
        key: (u64, u64, i32, i32),
    ) -> Option<()> {
        let import = self.import.borrow();
        let import = import.as_ref()?;
        let (source, content) = match frame_source {
            FrameSource::Magnifier(pixels) => (
                self.upload_pixels(pixels)?,
                SizeInt32 {
                    Width: pixels.width as i32,
                    Height: pixels.height as i32,
                },
            ),
            other => (other.texture().ok()?, other.content_size().ok()?),
        };
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
            let mut source_desc = D3D11_TEXTURE2D_DESC::default();
            // SAFETY: desc 为输出结构，由本次调用初始化。
            unsafe { source.GetDesc(&mut source_desc) };
            log::warn!(
                "背板捕获拷贝失败: {error}（local=({local_x},{local_y}) src_box=({},{},{},{}) content={}x{} 源纹理={}x{} 格式{:?} import={}x{} read_value={}）",
                source_box.left,
                source_box.top,
                source_box.right,
                source_box.bottom,
                content.Width,
                content.Height,
                source_desc.Width,
                source_desc.Height,
                source_desc.Format,
                import.size.0,
                import.size.1,
                request.read_value
            );
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

    /// 把放大镜像素帧上传为 GPU 纹理；纹理按需创建并随帧尺寸复用。
    fn upload_pixels(&self, pixels: &PixelFrame) -> Option<ID3D11Texture2D> {
        if self
            .upload
            .borrow()
            .as_ref()
            .is_none_or(|(_, width, height)| *width < pixels.width || *height < pixels.height)
        {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: pixels.width.max(1),
                Height: pixels.height.max(1),
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                ..Default::default()
            };
            let mut texture = None;
            // SAFETY: 描述符已初始化，输出 Option 由本次调用初始化并拥有。
            if let Err(error) =
                unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut texture)) }
            {
                log::warn!("放大镜像素上传纹理创建失败: {error}");
                return None;
            }
            *self.upload.borrow_mut() = Some((texture?, desc.Width, desc.Height));
        }
        let texture = self.upload.borrow().as_ref()?.0.clone();
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: pixels.width,
            bottom: pixels.height,
            back: 1,
        };
        // SAFETY: 拷贝盒位于纹理内；pixels 持有 width 行、每行 width*4 字节的 BGRA 数据。
        unsafe {
            self.context.UpdateSubresource(
                &texture,
                0,
                Some(&region),
                pixels.pixels.as_ptr().cast(),
                pixels.width * 4,
                0,
            );
        }
        Some(texture)
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
                // 共享写围栏的值由底层内核对象持有，跨捕获端实例单调递增；新建的
                // 捕获端必须从围栏当前值继续 Signal，从 0 重来会因值回退被拒
                // （E_INVALIDARG）——中途回退后端或重建捕获时必然触发
                // SAFETY: 围栏接口存活，仅读取其当前完成值。
                let fence_value = unsafe { import.write_fence.GetCompletedValue() };
                self.write_value.store(
                    fence_value.max(self.write_value.load(Ordering::Acquire)),
                    Ordering::Release,
                );
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
        if self.magnifier_mode.get() {
            match self.ensure_pixel_session(rate_hz) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    log::warn!("放大镜背景不可用，回退整屏捕获: {error}");
                    self.magnifier_mode.set(false);
                    self.session.borrow_mut().take();
                    *lock(&self.shared.latest) = Latest::default();
                    self.copied.set(None);
                    self.exclude_self();
                }
            }
        }
        // SAFETY: hwnd 是存活的悬浮窗；该调用只读取其位置。
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        if let Some(session) = self.session.borrow().as_ref()
            && session.monitor() == monitor
        {
            session.sync_rate(rate_hz);
            if let Err(error) = session.refresh() {
                self.session.borrow_mut().take();
                return Err(error);
            }
            return Ok(());
        }
        self.session.borrow_mut().take();
        *lock(&self.shared.latest) = Latest::default();
        // 一并清空 interest：DDA 轮询线程以「interest 为空」识别渲染端尚未确认
        // 新会话，从而走全帧兜底，避免陷入等 interest 才产 source 的死锁
        *lock(&self.shared.interest) = Rect::default();
        self.copied.set(None);
        let session = Session::start(
            self.backend,
            self.hwnd.0 as isize,
            monitor,
            &self.shared,
            rate_hz,
            &self.device,
            &self.context,
        )?;
        *self.session.borrow_mut() = Some(session);
        Ok(())
    }

    /// 兼容模式主路径：隐藏宿主经放大镜图像回调直接产出 BGRA 帧，不经过
    /// WGC 窗口捕获（避免系统捕获边框与宿主尺寸变化时重建帧池的卡顿）。
    fn ensure_pixel_session(&self, rate_hz: u32) -> Result<(), String> {
        let rate_hz = rate_hz.min(PIXEL_RATE_LIMIT);
        if let Some(Session::Magnifier(session)) = self.session.borrow().as_ref() {
            if session.rate_hz.get() != rate_hz {
                session.rate_hz.set(rate_hz);
                session.magnifier.set_rate(rate_hz);
            }
            return Ok(());
        }
        self.session.borrow_mut().take();
        *lock(&self.shared.latest) = Latest::default();
        self.copied.set(None);
        let weak = Arc::downgrade(&self.shared);
        let sink: FrameSink = Box::new(move |frame| {
            if let Some(shared) = weak.upgrade() {
                shared.receive_pixels(frame);
            }
        });
        let rect = window_rect(self.hwnd)?;
        let magnifier = Magnifier::start(rect, rate_hz, Some(sink))?;
        // SAFETY: hwnd 是存活的悬浮窗；该调用只读取其位置。
        let monitor = unsafe { MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST) };
        let origin = (rect.left, rect.top);
        *self.session.borrow_mut() = Some(Session::Magnifier(MagnifierSession {
            monitor,
            origin: Cell::new(origin),
            rate_hz: Cell::new(rate_hz),
            magnifier,
        }));
        Ok(())
    }

    /// 放大镜链路全部失效回退整屏时，由捕获端自行设置 WDA 排除（岛对捕获隐身）。
    fn exclude_self(&self) {
        crate::utils::liquid_glass::set_exclude_from_capture(
            windows::Win32::Foundation::HWND(self.hwnd.0),
            true,
        );
        self.self_excluded.set(true);
    }
}

impl WgcSession {
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
        let (pool, session, token, borderless) = Self::open(&item, shared, rate_hz)?;
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

    /// 从既有捕获项构建帧池与会话；整屏与放大镜宿主窗口捕捉共用。
    fn open(
        item: &GraphicsCaptureItem,
        shared: &Arc<Shared>,
        rate_hz: u32,
    ) -> Result<
        (
            Direct3D11CaptureFramePool,
            GraphicsCaptureSession,
            i64,
            bool,
        ),
        String,
    > {
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
            .CreateCaptureSession(item)
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
        Ok((pool, session, token, borderless))
    }
}

impl Drop for WgcSession {
    fn drop(&mut self) {
        let _ = self.pool.RemoveFrameArrived(self.token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

impl Shared {
    /// 放大镜像素回调（放大镜线程）：与上一帧完全相同则忽略，否则发布新帧。
    fn receive_pixels(&self, frame: PixelFrame) {
        {
            let mut latest = lock(&self.latest);
            if let Some(FrameSource::Magnifier(previous)) = latest.source.as_ref()
                && **previous == frame
            {
                return;
            }
            latest.source = Some(FrameSource::Magnifier(Arc::new(frame)));
            latest.sequence += 1;
        }
        if !self.pending.swap(true, Ordering::AcqRel) {
            crate::gpu::wake();
        }
    }

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
            latest.source = Some(FrameSource::Wgc(frame));
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
        self.upload.borrow_mut().take();
        // 仅清除捕获端回退时自设的 WDA；应用层管理的标志由应用层负责
        if self.self_excluded.get() {
            crate::utils::liquid_glass::set_exclude_from_capture(
                windows::Win32::Foundation::HWND(self.hwnd.0),
                false,
            );
        }
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

pub(super) fn intersects(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// 读取悬浮窗的屏幕矩形；矩形为空或不可用时返回错误。
fn window_rect(hwnd: HWND) -> Result<RECT, String> {
    let mut rect = RECT::default();
    // SAFETY: hwnd 是存活的悬浮窗且 rect 是有效输出。
    unsafe { windows62::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect) }
        .map_err(|error| format!("悬浮窗边界不可用: {error}"))?;
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return Err("悬浮窗边界为空".to_string());
    }
    Ok(rect)
}

/// 把屏幕矩形裁剪到悬浮窗所在显示器内；放大镜像素回调只产出屏幕内区域。
fn monitor_clamped(hwnd: HWND, rect: Rect) -> Option<RECT> {
    // SAFETY: hwnd 是存活的悬浮窗；该调用只读取其位置。
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: monitor 来自 MonitorFromWindow，info 是尺寸正确的输出结构。
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    let bounds = info.rcMonitor;
    let clamped = RECT {
        left: rect.left.max(bounds.left),
        top: rect.top.max(bounds.top),
        right: rect.right.min(bounds.right),
        bottom: rect.bottom.min(bounds.bottom),
    };
    (clamped.right > clamped.left && clamped.bottom > clamped.top).then_some(clamped)
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
