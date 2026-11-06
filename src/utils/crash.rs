//! 崩溃日志：把 panic 与未处理异常写入配置目录下的 crash.log，便于用户回传定位。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use windows62::Win32::Foundation::HMODULE;
use windows62::Win32::System::Diagnostics::Debug::{
    EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, RtlCaptureStackBackTrace,
    SetUnhandledExceptionFilter,
};
use windows62::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows62::core::PCWSTR;

/// 安装 panic 与未处理异常日志钩子；只记录，不改变默认终止行为。
pub fn install() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let mut text = String::new();
        text.push_str("== panic ==\n");
        text.push_str(&format!("thread: {name}\n"));
        text.push_str(&format!("{info}\n"));
        text.push_str(&format!(
            "backtrace:\n{}\n",
            std::backtrace::Backtrace::force_capture()
        ));
        append(&text);
    }));
    // SAFETY: 仅安装进程级异常过滤器；回调只读异常上下文并写日志。
    unsafe {
        SetUnhandledExceptionFilter(Some(filter));
    }
}

unsafe extern "system" fn filter(info: *const EXCEPTION_POINTERS) -> i32 {
    if info.is_null() {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    // SAFETY: info 由系统异常分发提供，仅在本次回调内读取；回读帧数组是本函数局部变量。
    unsafe {
        if (*info).ExceptionRecord.is_null() {
            return EXCEPTION_CONTINUE_SEARCH;
        }
        let record = (*info).ExceptionRecord;
        let code = (*record).ExceptionCode.0 as u32;
        let address = (*record).ExceptionAddress as usize;
        let mut frames = [std::ptr::null_mut(); 32];
        let count = RtlCaptureStackBackTrace(0, &mut frames, None);
        let mut text = String::new();
        text.push_str("== unhandled exception ==\n");
        text.push_str(&format!("code: 0x{code:08X}\n"));
        text.push_str(&format!("address: 0x{address:X}\n"));
        if let Some((module, offset)) = module_of(address) {
            text.push_str(&format!("location: {module}+0x{offset:X}\n"));
        }
        let magnifier = crate::gpu::magnifier_debug_state();
        if magnifier != [0; 6] {
            text.push_str(&format!(
                "magnifier: source=({},{},{},{}) size={}x{}\n",
                magnifier[0], magnifier[1], magnifier[2], magnifier[3], magnifier[4], magnifier[5]
            ));
        }
        for frame in frames.iter().take(count as usize) {
            let address = *frame as usize;
            if address == 0 {
                continue;
            }
            match module_of(address) {
                Some((module, offset)) => text.push_str(&format!("  {module}+0x{offset:X}\n")),
                None => text.push_str(&format!("  0x{address:X}\n")),
            }
        }
        append(&text);
    }
    EXCEPTION_CONTINUE_SEARCH
}

fn module_of(address: usize) -> Option<(String, usize)> {
    let mut module = HMODULE::default();
    // SAFETY: 仅按地址查询其所属模块，不增加引用计数。
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(address as *const u16),
            &mut module,
        )
        .ok()?;
    }
    let mut buffer = [0u16; 260];
    // SAFETY: buffer 是长度正确的输出缓冲区。
    let length = unsafe { GetModuleFileNameW(Some(module), &mut buffer) } as usize;
    let length = length.min(buffer.len());
    Some((
        String::from_utf16_lossy(&buffer[..length]),
        address.wrapping_sub(module.0 as usize),
    ))
}

fn log_path() -> PathBuf {
    let mut path = crate::core::persistence::get_config_path();
    path.pop();
    path.push("crash.log");
    path
}

fn append(text: &str) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = file.write_all(text.as_bytes());
        let _ = file.write_all(b"\n");
        let _ = file.flush();
    }
}
