use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use base64::Engine;
use serde::Serialize;
use tauri::Emitter;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{MUTEX_ALL_ACCESS, OpenMutexW};
use windows::core::w;

use crate::core::config::{APP_AUTHOR, APP_HOMEPAGE, APP_VERSION, AppConfig};
use crate::core::i18n::{all_translations, current_lang, init_i18n, tr};
use crate::core::persistence::{load_config, save_config, with_config_lock};
use crate::utils::font::{FontManager, can_load_font_file};

pub(crate) const SETTINGS_TITLE: &str = "EchoMusic-Lyrics-WinIsland Settings";
const APP_ICON_PNG: &[u8] = include_bytes!("../../../resources/icon.png");
const APP_ICON_DARK_PNG: &[u8] = include_bytes!("../../../resources/icon-dark.png");

#[derive(Serialize)]
struct AppInfo {
    version: &'static str,
    author: &'static str,
    homepage: &'static str,
    icon_png: Vec<u8>,
    icon_dark_png: Vec<u8>,
}

#[derive(Serialize)]
struct SettingsState {
    config: AppConfig,
    current_lang: String,
    translations: HashMap<String, String>,
    monitors: Vec<String>,
    app: AppInfo,
    lyrics_filter_regex_valid: bool,
}

#[tauri::command]
fn get_settings_state() -> Result<SettingsState, String> {
    Ok(build_state(load_config()))
}

#[tauri::command]
fn save_settings_config(config: AppConfig) -> Result<SettingsState, String> {
    let state = with_config_lock(|| {
        let old_config = load_config();
        let config = config.normalized();
        apply_config_side_effects(&old_config, &config);
        save_config(&config);
        config
    });
    Ok(build_state(state))
}

#[tauri::command]
fn reset_settings() -> Result<SettingsState, String> {
    let config = with_config_lock(|| {
        let config = AppConfig::default().normalized();
        save_config(&config);
        config
    });
    init_i18n(&config.language);
    FontManager::global().refresh_custom_font();
    Ok(build_state(config))
}

#[tauri::command]
async fn select_font_file() -> Result<SettingsState, String> {
    let picked = rfd::AsyncFileDialog::new()
        .add_filter("Fonts", &["ttf", "otf"])
        .pick_file()
        .await;
    let Some(picked) = picked else {
        return Ok(build_state(load_config()));
    };
    let path = picked.path().to_string_lossy().into_owned();
    if !can_load_font_file(&path) {
        return Err(tr("font_load_failed"));
    }
    let config = with_config_lock(|| {
        let mut config = load_config();
        config.custom_font_path = Some(path);
        let config = config.normalized();
        save_config(&config);
        config
    });
    FontManager::global().refresh_custom_font();
    Ok(build_state(config))
}

#[tauri::command]
fn reset_font() -> Result<SettingsState, String> {
    let config = with_config_lock(|| {
        let mut config = load_config();
        config.custom_font_path = None;
        let config = config.normalized();
        save_config(&config);
        config
    });
    FontManager::global().refresh_custom_font();
    Ok(build_state(config))
}

#[tauri::command]
fn check_updates_now() -> Result<(), String> {
    // 更新检查与安装统一由主进程编排：设置进程只投递请求信号，
    // 避免设置进程自身成为替换目标的占用者
    crate::utils::updater::request_update_now();
    Ok(())
}

#[tauri::command]
fn open_homepage() -> Result<(), String> {
    open::that(APP_HOMEPAGE).map_err(|err| err.to_string())
}

#[tauri::command]
fn get_builtin_font() -> Option<String> {
    crate::utils::font::get_builtin_font_data()
        .map(|data| base64::engine::general_purpose::STANDARD.encode(data))
}

#[tauri::command]
fn get_custom_font() -> Option<String> {
    crate::utils::font::get_custom_font_data()
        .map(|data| base64::engine::general_purpose::STANDARD.encode(data))
}

pub fn run_settings(config: AppConfig) {
    init_i18n(&config.language);

    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            watch_main_instance(handle.clone());
            watch_config_changes(handle);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_settings_state,
            save_settings_config,
            reset_settings,
            select_font_file,
            reset_font,
            check_updates_now,
            open_homepage,
            get_builtin_font,
            get_custom_font,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Tauri settings window");
}

pub fn bring_settings_to_front() {
    crate::utils::win32::bring_window_to_front(SETTINGS_TITLE);
}

pub(crate) fn close_settings_window() {
    crate::utils::win32::close_window(SETTINGS_TITLE);
}

/// 以独立进程打开设置窗口（设置窗口与主进程各自持有互斥体，互不阻塞）。
pub(crate) fn open_settings_window() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).arg("--settings").spawn();
    }
}

fn build_state(config: AppConfig) -> SettingsState {
    SettingsState {
        lyrics_filter_regex_valid: regex::Regex::new(&config.lyrics_filter_regex).is_ok(),
        config,
        current_lang: current_lang(),
        translations: all_translations(),
        monitors: get_monitor_list(),
        app: AppInfo {
            version: APP_VERSION,
            author: APP_AUTHOR,
            homepage: APP_HOMEPAGE,
            icon_png: APP_ICON_PNG.to_vec(),
            icon_dark_png: APP_ICON_DARK_PNG.to_vec(),
        },
    }
}

fn apply_config_side_effects(old_config: &AppConfig, config: &AppConfig) {
    if old_config.language != config.language {
        init_i18n(&config.language);
    }
    if old_config.custom_font_path != config.custom_font_path {
        FontManager::global().refresh_custom_font();
    }
    // 开机自启的注册表副作用统一由主进程配置热重载执行，设置进程只负责落盘
}

fn watch_config_changes(app_handle: tauri::AppHandle) {
    thread::spawn(move || {
        let mut initialized = false;
        let mut last_mtime = None;
        loop {
            thread::sleep(Duration::from_millis(500));
            if !main_instance_exists() {
                break;
            }
            // 先比较文件修改时间，未变化时跳过整份状态构建（图标数据与显示器枚举开销不小）
            let mtime = std::fs::metadata(crate::core::persistence::get_config_path())
                .and_then(|meta| meta.modified())
                .ok();
            if initialized && mtime == last_mtime {
                continue;
            }
            last_mtime = mtime;
            if !initialized {
                initialized = true;
                continue;
            }
            // 主进程或 WebSocket 通道改动了配置，推送全量状态让设置页保持同步
            let _ = app_handle.emit("config-changed", &build_state(load_config()));
        }
    });
}

fn watch_main_instance(app_handle: tauri::AppHandle) {
    thread::spawn(move || {
        loop {
            if !main_instance_exists() {
                app_handle.exit(0);
                break;
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

fn main_instance_exists() -> bool {
    // SAFETY: OpenMutexW 只按静态名称打开已存在的互斥体；成功后立即关闭句柄。
    unsafe {
        match OpenMutexW(
            MUTEX_ALL_ACCESS,
            false,
            w!("Local\\EchoMusic_Lyrics_WinIsland_SingleInstance_Mutex"),
        ) {
            Ok(handle) => {
                let _ = CloseHandle(handle);
                true
            }
            Err(_) => false,
        }
    }
}

fn get_monitor_list() -> Vec<String> {
    use windows::Win32::Graphics::Gdi::{
        DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICEW, EnumDisplayDevicesW,
    };

    let mut monitors: Vec<String> = Vec::new();

    // SAFETY: EnumDisplayDevicesW 使用栈上初始化的 DISPLAY_DEVICEW 结构体并按文档设置 cb 字段；
    // 设备名来自上一轮系统调用返回的固定缓冲区，调用期间保持有效。
    unsafe {
        let mut idx = 0u32;
        let mut active_count = 0;
        loop {
            let mut display_device: DISPLAY_DEVICEW = std::mem::zeroed();
            display_device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
            if !EnumDisplayDevicesW(None, idx, &mut display_device, 0).as_bool() {
                break;
            }

            if (display_device.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0 {
                active_count += 1;
                let name = String::from_utf16_lossy(&display_device.DeviceName)
                    .trim_end_matches('\0')
                    .to_string();
                let mut monitor_device: DISPLAY_DEVICEW = std::mem::zeroed();
                monitor_device.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
                let mut label = if EnumDisplayDevicesW(
                    windows::core::PCWSTR(display_device.DeviceName.as_ptr()),
                    0,
                    &mut monitor_device,
                    0,
                )
                .as_bool()
                {
                    let friendly = String::from_utf16_lossy(&monitor_device.DeviceString)
                        .trim_end_matches('\0')
                        .to_string();
                    if friendly.is_empty() {
                        name.clone()
                    } else {
                        friendly
                    }
                } else {
                    name.clone()
                };
                label = tr("monitor_label")
                    .replace("{index}", &active_count.to_string())
                    .replace("{name}", &label);
                monitors.push(label);
            }

            idx += 1;
        }
    }

    if monitors.is_empty() {
        monitors.push(tr("monitor_primary"));
    }

    monitors
}
