#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod core;
mod gpu;
mod icons;
mod ui;
mod utils;
mod window;

use std::env;
use std::mem::ManuallyDrop;

use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
use windows::Win32::Foundation::{CloseHandle, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;
use winit::event_loop::EventLoop;

use crate::core::i18n::init_i18n;
use crate::window::app::App;

fn main() {
    let _ = env_logger::try_init();
    utils::crash::install();
    let config = core::persistence::load_config();
    init_i18n(&config.language);

    let args: Vec<String> = env::args().collect();
    let is_restart = args.iter().any(|arg| arg == "--restart");

    if args.iter().any(|arg| arg == "--settings") {
        let _settings_mutex;
        // SAFETY: CreateMutexW creates a named mutex for single-instance enforcement.
        // The name is a static string literal. GetLastError checks if the mutex
        // already exists (ERROR_ALREADY_EXISTS) to bring existing window to front.
        unsafe {
            _settings_mutex = CreateMutexW(
                None,
                true,
                w!("Local\\EchoMusic_Lyrics_WinIsland_Settings_Mutex"),
            );
            if GetLastError() == ERROR_ALREADY_EXISTS {
                crate::window::settings::bring_settings_to_front();
                return;
            }
        }
        crate::window::settings::run_settings(config);
    } else {
        if is_restart {
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
        let _single_mutex = {
            let start = std::time::Instant::now();
            loop {
                // SAFETY: CreateMutexW creates a named mutex for single-instance lock.
                // The name is a static string literal. On success with no ERROR_ALREADY_EXISTS,
                // the handle is kept in ManuallyDrop to hold the lock for the process lifetime.
                // On ERROR_ALREADY_EXISTS, the handle is closed and we retry or exit.
                unsafe {
                    let h = CreateMutexW(
                        None,
                        true,
                        w!("Local\\EchoMusic_Lyrics_WinIsland_SingleInstance_Mutex"),
                    );
                    match h {
                        Ok(handle) => {
                            if GetLastError() != ERROR_ALREADY_EXISTS {
                                break ManuallyDrop::new(handle);
                            }
                            let _ = CloseHandle(handle);
                        }
                        Err(_) => {
                            if !is_restart {
                                return;
                            }
                        }
                    }
                }
                if !is_restart || start.elapsed() > std::time::Duration::from_secs(10) {
                    if is_restart {
                        // 兜底只清理主实例进程：按命令行排除 --settings 进程，
                        // 避免强制误杀独立运行的设置窗口（设置窗口监听主实例互斥体后会自行退出）
                        let own_pid = std::process::id();
                        let exe_name = std::env::current_exe()
                            .ok()
                            .and_then(|path| {
                                path.file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                            })
                            .unwrap_or_else(|| "EchoMusic-Lyrics-WinIsland.exe".to_string());
                        if let Ok(output) = std::process::Command::new("powershell")
                            .args([
                                "-NoProfile",
                                "-Command",
                                &format!(
                                    "Get-CimInstance Win32_Process -Filter \"Name='{}'\" | \
                                     Where-Object {{$_.ProcessId -ne {own_pid} -and $_.CommandLine -notmatch '--settings'}} | \
                                     ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}",
                                    exe_name.replace('\'', "''")
                                ),
                            ])
                            .output()
                            && output.status.success()
                        {
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            continue;
                        }
                    }
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        };

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let guard = runtime.enter();

        utils::updater::start_update_checker();

        let event_loop = EventLoop::new().unwrap();
        // 注册唤醒代理：屏幕捕获回调通过它请求主线程重绘
        crate::gpu::set_wake_proxy(event_loop.create_proxy());
        let mut app = App::default();
        let result = event_loop.run_app(&mut app);
        let restart_requested = app.take_restart_requested();

        drop(app);
        drop(guard);
        drop(runtime);

        result.unwrap();

        if restart_requested && let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).arg("--restart").spawn();
        }
    }
}
