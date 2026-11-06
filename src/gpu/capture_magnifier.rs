//! Windows 放大镜（Magnification API）背景采集，液态玻璃-WGC 兼容模式的来源。
//!
//! 放大镜宿主窗口渲染 `source` 区域下方的桌面内容，并通过窗口过滤列表排除
//! 本进程的所有窗口。帧经隐藏宿主的图像回调（sink）直接送达；兼容模式不再
//! 由 WGC 捕获宿主窗口，避免系统捕获边框与宿主尺寸变化时重建帧池的卡顿。
//!
//! 移植自上游 WinIsland 的 capture/magnifier.rs。

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows62::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows62::Win32::Graphics::Dwm::{DWMWA_CLOAK, DwmSetWindowAttribute};
use windows62::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HRGN, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromRect,
};
use windows62::Win32::System::LibraryLoader::GetModuleHandleW;
use windows62::Win32::System::Threading::GetCurrentProcessId;
use windows62::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows62::Win32::UI::Magnification::{
    MAGIMAGEHEADER, MAGTRANSFORM, MW_FILTERMODE_EXCLUDE, MagInitialize, MagSetImageScalingCallback,
    MagSetWindowFilterList, MagSetWindowSource, MagSetWindowTransform, MagUninitialize,
    WC_MAGNIFIERW,
};
use windows62::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EnumWindows,
    GetWindowThreadProcessId, IsWindowVisible, LWA_ALPHA, MSG, PM_REMOVE, PeekMessageW,
    RegisterClassW, SWP_NOACTIVATE, SWP_NOREDRAW, SWP_NOZORDER, SetLayeredWindowAttributes,
    SetWindowPos, TranslateMessage, WINDOW_EX_STYLE, WNDCLASSW, WS_CHILD, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE,
};
use windows62::core::{BOOL, PCWSTR, w};

use super::capture::lock;

const FILTER_REFRESH: Duration = Duration::from_millis(500);

/// 最近一次提交给放大镜的源矩形与控件尺寸（左、上、右、下、宽、高），
/// 供崩溃日志判断 magnification.dll 是否在越界源上崩溃。
static DEBUG_STATE: [AtomicI32; 6] = [
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
    AtomicI32::new(0),
];

pub(super) fn debug_state() -> [i32; 6] {
    DEBUG_STATE
        .each_ref()
        .map(|value| value.load(Ordering::Relaxed))
}

/// 放大镜图像回调产出的一帧：紧凑排列的 BGRA 行（头部声明 RGBA，但字节实为 BGRA）。
#[derive(PartialEq)]
pub(super) struct PixelFrame {
    pub(super) pixels: Vec<u8>,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) origin: (i32, i32),
}

pub(super) type FrameSink = Box<dyn Fn(PixelFrame) + Send>;

thread_local! {
    static SINK: RefCell<Option<(FrameSink, (i32, i32))>> = const { RefCell::new(None) };
}

struct State {
    source: Mutex<RECT>,
    rate_hz: AtomicU32,
    stop: AtomicBool,
}

/// 渲染 `source` 下方桌面内容（不含本进程窗口）的放大镜，帧经隐藏宿主的
/// 图像回调送达。不带 sink 时宿主被 cloak 并可作为普通窗口被 WGC 捕捉，
/// 该路径当前不用于兼容模式。
pub(super) struct Magnifier {
    state: Arc<State>,
    thread: Option<JoinHandle<()>>,
}

impl Magnifier {
    pub(super) fn start(
        source: RECT,
        rate_hz: u32,
        sink: Option<FrameSink>,
    ) -> Result<Self, String> {
        let state = Arc::new(State {
            source: Mutex::new(source),
            rate_hz: AtomicU32::new(rate_hz.max(1)),
            stop: AtomicBool::new(false),
        });
        let (sender, receiver) = std::sync::mpsc::channel();
        let thread_state = state.clone();
        let thread = std::thread::Builder::new()
            .name("liquid-magnifier".into())
            .spawn(move || run(thread_state, sender, sink))
            .map_err(|error| format!("放大镜线程启动失败: {error}"))?;
        match receiver.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(_)) => Ok(Self {
                state,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // 初始化卡住时放弃等待并让线程自行退出，避免渲染线程被永久阻塞
                state.stop.store(true, Ordering::Release);
                Err("放大镜线程初始化超时".to_string())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = thread.join();
                Err("放大镜线程在初始化期间退出".to_string())
            }
        }
    }

    pub(super) fn set_source(&self, source: RECT) {
        *lock(&self.state.source) = source;
    }

    pub(super) fn set_rate(&self, rate_hz: u32) {
        self.state.rate_hz.store(rate_hz.max(1), Ordering::Relaxed);
    }
}

impl Drop for Magnifier {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

extern "system" fn host_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: 将未经修改的消息转发给默认窗口过程。
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn on_frame(
    _hwnd: HWND,
    source: *mut c_void,
    header: MAGIMAGEHEADER,
    _destination: *mut c_void,
    _destination_header: MAGIMAGEHEADER,
    _unclipped: RECT,
    _clipped: RECT,
    _dirty: HRGN,
) -> BOOL {
    let row = header.width as usize * 4;
    let stride = header.stride as usize;
    let height = header.height as usize;
    let end = (header.offset as usize)
        .saturating_add(stride.saturating_mul(height.saturating_sub(1)))
        .saturating_add(row);
    if source.is_null() || row == 0 || height == 0 || stride < row || end > header.cbSize {
        return BOOL(1);
    }
    SINK.with(|slot| {
        let slot = slot.borrow();
        let Some((sink, origin)) = slot.as_ref() else {
            return;
        };
        let mut pixels = Vec::with_capacity(row * header.height as usize);
        for y in 0..header.height as usize {
            // SAFETY: 放大镜保证 offset 之后有 height 行 stride 字节，且每行
            // 拷贝都停留在其 stride 范围内。
            pixels.extend_from_slice(unsafe {
                std::slice::from_raw_parts(
                    source
                        .cast::<u8>()
                        .add(header.offset as usize + y * header.stride as usize),
                    row,
                )
            });
        }
        sink(PixelFrame {
            pixels,
            width: header.width,
            height: header.height,
            origin: *origin,
        });
    });
    BOOL(1)
}

fn run(
    state: Arc<State>,
    ready: std::sync::mpsc::Sender<Result<isize, String>>,
    sink: Option<FrameSink>,
) {
    let callback = sink.is_some();
    // SAFETY: 以下所有句柄都在本线程创建、使用并销毁；放大镜运行时在创建
    // 控件之前初始化，在控件销毁之后释放。
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if !MagInitialize().as_bool() {
            let _ = ready.send(Err("MagInitialize 失败".to_string()));
            return;
        }
        let setup = (|| -> Result<(HWND, HWND), String> {
            let instance = GetModuleHandleW(None).map_err(|error| error.to_string())?;
            let class = w!("EchoMusicLyricsMagnifierHost");
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(host_proc),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            });
            let source = lock(&state.source);
            let host = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("EchoMusic Liquid Backdrop"),
                if callback {
                    WS_POPUP
                } else {
                    WS_POPUP | WS_VISIBLE
                },
                source.left,
                source.top,
                (source.right - source.left).max(1),
                (source.bottom - source.top).max(1),
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|error| format!("放大镜宿主窗口创建失败: {error}"))?;
            let _ = SetLayeredWindowAttributes(host, COLORREF(0), 255, LWA_ALPHA);
            if !callback {
                let cloak = 1_i32;
                let _ = DwmSetWindowAttribute(
                    host,
                    DWMWA_CLOAK,
                    (&cloak as *const i32).cast(),
                    std::mem::size_of::<i32>() as u32,
                );
            }
            let magnifier = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(WC_MAGNIFIERW.as_ptr()),
                w!("EchoMusic Liquid Magnifier"),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                (source.right - source.left).max(1),
                (source.bottom - source.top).max(1),
                Some(host),
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|error| {
                let _ = DestroyWindow(host);
                format!("放大镜控件创建失败: {error}")
            })?;
            let mut transform = MAGTRANSFORM::default();
            transform.v[0] = 1.0;
            transform.v[4] = 1.0;
            transform.v[8] = 1.0;
            let _ = MagSetWindowTransform(magnifier, &mut transform);
            Ok((host, magnifier))
        })();
        let (host, magnifier) = match setup {
            Ok(handles) => handles,
            Err(error) => {
                let _ = MagUninitialize();
                let _ = ready.send(Err(error));
                return;
            }
        };
        if let Some(sink) = sink {
            let source = lock(&state.source);
            SINK.with(|slot| *slot.borrow_mut() = Some((sink, (source.left, source.top))));
            if !MagSetImageScalingCallback(magnifier, Some(on_frame)).as_bool() {
                log::warn!("放大镜图像回调注册失败，像素帧将不可用");
            }
        }
        let mut excluded = own_windows();
        let mut filter_ok = apply_filter(magnifier, &excluded, true);
        let _ = ready.send(Ok(host.0 as isize));

        let mut size = (0, 0);
        let mut last_current = (0, 0);
        let mut initialized = false;
        let mut next = Instant::now();
        let mut filter_at = Instant::now();
        let mut refilter = false;
        let mut message = MSG::default();
        while !state.stop.load(Ordering::Acquire) {
            // 立即复制并释放锁：消息泵、放大镜渲染与帧间休眠期间不能持有
            // 源矩形锁，否则渲染线程的 set_source 会被阻塞到本帧结束。
            let source = *lock(&state.source);
            // 源矩形必须落在显示器内：部分系统版本会在 magnification.dll 内部
            // 拷贝屏幕外像素时越界崩溃。完全离屏时保持上一帧，不读取源。
            if let Some(source) = clamped_source(source) {
                let current = (source.right - source.left, source.bottom - source.top);
                if !initialized {
                    // 首帧只建立控件尺寸：放大镜控件完成消息初始化前读取源会在
                    // magnification.dll 内部越界拷贝并崩溃。
                    initialized = true;
                    size = current;
                    last_current = current;
                    let _ = SetWindowPos(
                        host,
                        None,
                        source.left,
                        source.top,
                        size.0,
                        size.1,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW,
                    );
                    let _ = SetWindowPos(
                        magnifier,
                        None,
                        0,
                        0,
                        size.0,
                        size.1,
                        SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW,
                    );
                    refilter = true;
                } else {
                    // 尺寸变化期间先用旧控件尺寸继续渲染，保持图像回调帧流；
                    // 帧流中断会让渲染端回退到 CPU 截屏，而兼容模式下岛对捕获
                    // 可见，截屏会把岛自身计入背景（玻璃自叠加）并拖慢主线程。
                    // 尺寸稳定后再重排控件，此时源与控件尺寸一致，避免在
                    // Windows 11 23H2 的 magnification.dll 内部越界崩溃。
                    if current != last_current {
                        last_current = current;
                    } else if current != size {
                        size = current;
                        let _ = SetWindowPos(
                            host,
                            None,
                            source.left,
                            source.top,
                            size.0,
                            size.1,
                            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW,
                        );
                        let _ = SetWindowPos(
                            magnifier,
                            None,
                            0,
                            0,
                            size.0,
                            size.1,
                            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOREDRAW,
                        );
                        refilter = true;
                    }
                    SINK.with(|slot| {
                        if let Some((_, origin)) = slot.borrow_mut().as_mut() {
                            *origin = (source.left, source.top);
                        }
                    });
                    DEBUG_STATE[0].store(source.left, Ordering::Relaxed);
                    DEBUG_STATE[1].store(source.top, Ordering::Relaxed);
                    DEBUG_STATE[2].store(source.right, Ordering::Relaxed);
                    DEBUG_STATE[3].store(source.bottom, Ordering::Relaxed);
                    DEBUG_STATE[4].store(size.0, Ordering::Relaxed);
                    DEBUG_STATE[5].store(size.1, Ordering::Relaxed);
                    let _ = MagSetWindowSource(magnifier, source);
                    let _ = InvalidateRect(Some(magnifier), None, false);
                }
            }
            let windows = own_windows();
            if refilter || windows != excluded || filter_at.elapsed() >= FILTER_REFRESH {
                refilter = false;
                filter_at = Instant::now();
                excluded = windows;
                filter_ok = apply_filter(magnifier, &excluded, filter_ok);
            }
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let rate = state.rate_hz.load(Ordering::Relaxed).max(1);
            next += Duration::from_nanos(1_000_000_000 / u64::from(rate));
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
        let _ = DestroyWindow(host);
        let _ = MagUninitialize();
        SINK.with(|slot| slot.borrow_mut().take());
    }
}

/// 把源矩形裁剪到其所在显示器。放大镜源越出屏幕时会读取屏外像素，部分系统
/// 版本（如 Windows 11 23H2）在 magnification.dll 内部越界拷贝并崩溃；完全
/// 离屏时返回 None，调用方保持上一帧。
fn clamped_source(source: RECT) -> Option<RECT> {
    // SAFETY: 仅按矩形查询所在显示器并读取其边界。
    let monitor = unsafe { MonitorFromRect(&source, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: monitor 来自系统调用，info 是尺寸正确的输出结构。
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Some(source);
    }
    let bounds = info.rcMonitor;
    let clamped = RECT {
        left: source.left.max(bounds.left),
        top: source.top.max(bounds.top),
        right: source.right.min(bounds.right),
        bottom: source.bottom.min(bounds.bottom),
    };
    (clamped.right > clamped.left && clamped.bottom > clamped.top).then_some(clamped)
}

/// 重新发送排除列表。窗口隐藏后再次显示或放大镜尺寸变化时 DWM 可能丢弃某窗口
/// 的排除项，因此每次变化和定时都会重发。返回调用是否成功，开始失败时告警一次。
fn apply_filter(magnifier: HWND, windows: &[(HWND, bool)], was_ok: bool) -> bool {
    let mut handles: Vec<HWND> = windows.iter().map(|(hwnd, _)| *hwnd).collect();
    if handles.is_empty() {
        return was_ok;
    }
    // SAFETY: 放大镜控件在本线程存活，列表在调用期间保持有效。
    let ok = unsafe {
        MagSetWindowFilterList(
            magnifier,
            MW_FILTERMODE_EXCLUDE,
            handles.len() as i32,
            handles.as_mut_ptr(),
        )
    }
    .as_bool();
    if was_ok && !ok {
        log::warn!("放大镜无法排除本进程窗口");
    }
    ok
}

fn own_windows() -> Vec<(HWND, bool)> {
    extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        // SAFETY: data 在 EnumWindows 期间指向 own_windows 持有的 Vec。
        unsafe {
            let windows = &mut *(data.0 as *mut Vec<(HWND, bool)>);
            let mut process = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut process));
            if process == GetCurrentProcessId() {
                windows.push((hwnd, IsWindowVisible(hwnd).as_bool()));
            }
        }
        BOOL(1)
    }
    let mut windows = Vec::new();
    // SAFETY: 回调只向经 LPARAM 传入的局部 Vec 追加元素。
    unsafe {
        let _ = EnumWindows(
            Some(visit),
            LPARAM(&mut windows as *mut Vec<(HWND, bool)> as isize),
        );
    }
    windows
}
