use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::core::config::AppConfig;

#[cfg(windows)]
const CONFIG_LOCK_TIMEOUT_MS: u32 = 1_000;

pub fn get_config_path() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".echomusic-lyrics-winisland");
    if !path.exists() {
        let _ = fs::create_dir_all(&path);
    }
    path.push("config.toml");
    path
}

/// 跨进程配置写入互斥锁：主进程（WebSocket 通道）与设置进程对同一配置文件的
/// 读-改-写序列都必须持锁进行，避免并发互相覆盖丢更新。
/// 获取超时时不阻塞调用方（记录日志后直接继续），宁可丢一次更新也不能卡死渲染线程。
#[cfg(windows)]
pub fn with_config_lock<T>(f: impl FnOnce() -> T) -> T {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
    use windows::core::w;

    struct ConfigLockGuard(HANDLE);
    impl Drop for ConfigLockGuard {
        fn drop(&mut self) {
            // SAFETY: 句柄来自本函数内 CreateMutexW 成功返回，且 WaitForSingleObject
            // 已确认本线程持有互斥体；释放后立即关闭句柄，不与其他线程共享。
            unsafe {
                let _ = ReleaseMutex(self.0);
                let _ = CloseHandle(self.0);
            }
        }
    }

    // SAFETY: CreateMutexW 使用静态名称创建命名互斥体；句柄仅在本函数作用域内使用。
    let handle = unsafe {
        CreateMutexW(
            None,
            false,
            w!(r"Local\EchoMusic_Lyrics_WinIsland_ConfigMutex"),
        )
    };
    match handle {
        Ok(handle) => {
            // SAFETY: WaitForSingleObject 只等待本函数创建的互斥体句柄。
            let wait = unsafe { WaitForSingleObject(handle, CONFIG_LOCK_TIMEOUT_MS) };
            if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
                let _guard = ConfigLockGuard(handle);
                f()
            } else {
                // SAFETY: 获取锁失败路径，句柄未被持有，直接关闭。
                unsafe {
                    let _ = CloseHandle(handle);
                }
                log::warn!("配置文件写入锁获取超时，本次写入放弃互斥保护");
                f()
            }
        }
        Err(err) => {
            log::warn!("配置文件写入锁创建失败: {err}");
            f()
        }
    }
}

#[cfg(not(windows))]
pub fn with_config_lock<T>(f: impl FnOnce() -> T) -> T {
    f()
}

pub fn load_config() -> AppConfig {
    let path = get_config_path();
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return AppConfig::default().normalized(),
    };
    match toml::from_str::<AppConfig>(&content) {
        Ok(config) => config.normalized(),
        Err(err) => {
            // 读失败即毁源：解析失败绝不把默认配置写回磁盘，保留原文件为 .bak
            // 供用户/诊断找回，内存中使用默认配置。
            log::error!("配置文件解析失败，已保留为 config.toml.bak 并使用默认配置: {err}");
            let bak = path.with_extension("toml.bak");
            let _ = fs::rename(&path, &bak);
            AppConfig::default().normalized()
        }
    }
}

pub fn save_config(config: &AppConfig) {
    let Ok(content) = toml::to_string_pretty(config) else {
        log::error!("配置序列化失败，放弃本次写入");
        return;
    };
    let path = get_config_path();
    let tmp = path.with_extension("toml.tmp");
    let result = fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(content.as_bytes())?;
        file.sync_all()
    });
    match result {
        Ok(()) => {
            if let Err(err) = fs::rename(&tmp, &path) {
                log::error!("配置原子替换失败: {err}");
                let _ = fs::remove_file(&tmp);
            }
        }
        Err(err) => {
            log::error!("配置临时文件写入失败: {err}");
            let _ = fs::remove_file(&tmp);
        }
    }
}
