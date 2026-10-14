//! 宿主背板的窗口样式开关（移植自 WinIsland 的 window/styles.rs 中相关部分）。

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{BOOL, s, w};

type SetWindowCompositionAttribute =
    unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> BOOL;

#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WindowCompositionAttributeData {
    attribute: u32,
    data: *mut c_void,
    size: usize,
}

/// 开启 DWM 宿主背板画笔；系统不支持时回退到旧版亚克力模糊。
pub(super) fn enable_host_backdrop(hwnd: HWND) -> bool {
    if enable_host_backdrop_brush(hwnd) {
        return true;
    }
    let Some(set_attribute) = set_window_composition_attribute() else {
        return false;
    };
    // WCA_ACCENT_POLICY + ACCENT_ENABLE_ACRYLICBLURBEHIND：Win11 之前系统的亚克力回退
    let mut policy = AccentPolicy {
        state: 5,
        flags: 0,
        gradient_color: 0,
        animation_id: 0,
    };
    let mut data = WindowCompositionAttributeData {
        attribute: 19,
        data: (&mut policy as *mut AccentPolicy).cast(),
        size: size_of::<AccentPolicy>(),
    };
    // SAFETY: 动态解析出的函数使用上面校验过的原生 ABI；hwnd 存活，
    // data 指向本次调用期间有效的 accent policy。
    unsafe { set_attribute(hwnd, &mut data).as_bool() }
}

fn enable_host_backdrop_brush(hwnd: HWND) -> bool {
    let enabled: i32 = 1;
    // SAFETY: hwnd 属于存活窗口，enabled 指向同步 DWM 调用期间有效的 BOOL 值。
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_HOSTBACKDROPBRUSH,
            &enabled as *const _ as *const c_void,
            size_of::<i32>() as u32,
        )
    }
    .is_ok()
}

fn set_window_composition_attribute() -> Option<SetWindowCompositionAttribute> {
    static FUNCTION: OnceLock<Option<SetWindowCompositionAttribute>> = OnceLock::new();
    *FUNCTION.get_or_init(|| {
        // SAFETY: user32.dll 在任何 GUI 进程中都已被加载，导出地址在进程生命周期内有效，
        // 这里按原生 SetWindowCompositionAttribute 签名转换。
        unsafe {
            let module = GetModuleHandleW(w!("user32.dll")).ok()?;
            let function = GetProcAddress(module, s!("SetWindowCompositionAttribute"))?;
            Some(std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                SetWindowCompositionAttribute,
            >(function))
        }
    })
}
