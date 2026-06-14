use std::path::Path;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowThreadProcessId,
};

pub fn get_foreground_process_name() -> Option<String> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }

        let mut class_name = [0u16; 256];
        let len = GetClassNameW(hwnd, &mut class_name);
        if len > 0 {
            let name = String::from_utf16_lossy(&class_name[..len as usize]);
            if name == "Progman" || name == "WorkerW" || name == "Shell_TrayWnd" {
                return None;
            }
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return None;
        }

        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buf = [0u16; 260];
        let mut size = buf.len() as u32;

        // SAFETY: OpenProcess 返回的句柄有效，缓冲区在栈上分配且大小正确。
        // QueryFullProcessImageNameW 会写入缓冲区并更新 size。
        let result = QueryFullProcessImageNameW(
            handle,
            windows::Win32::System::Threading::PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );

        let _ = CloseHandle(handle);

        if result.is_err() || size == 0 {
            return None;
        }

        let path = String::from_utf16_lossy(&buf[..size as usize]);
        Some(
            Path::new(&path)
                .file_name()?
                .to_string_lossy()
                .to_lowercase()
                .to_string(),
        )
    }
}
