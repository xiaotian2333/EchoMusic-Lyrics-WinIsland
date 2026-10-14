use std::collections::HashMap;
use std::thread;
use std::time::Duration;

use base64::Engine;
use serde::Serialize;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{MUTEX_ALL_ACCESS, OpenMutexW};
use windows::core::w;

use crate::core::config::{APP_AUTHOR, APP_HOMEPAGE, APP_VERSION, AppConfig, is_valid_color};
use crate::core::i18n::{current_lang, init_i18n, tr};
use crate::core::persistence::{load_config, save_config};
use crate::utils::autostart::set_autostart;
use crate::utils::font::{FontManager, can_load_font_file};

const SETTINGS_TITLE: &str = "EchoMusic-Lyrics-WinIsland Settings";
const APP_ICON_PNG: &[u8] = include_bytes!("../../../resources/icon.png");
const APP_ICON_DARK_PNG: &[u8] = include_bytes!("../../../resources/icon-dark.png");

const TRANSLATION_KEYS: &[&str] = &[
    "tab_general",
    "tab_music",
    "tab_about",
    "section_appearance",
    "section_effects",
    "settings_theme",
    "theme_system",
    "theme_light",
    "theme_dark",
    "mini_cover_shape",
    "expanded_cover_shape",
    "shape_square",
    "shape_circle",
    "section_behavior",
    "section_updates",
    "section_lyrics",
    "non_expanded_scale",
    "expanded_scale",
    "base_width",
    "base_height",
    "expanded_width",
    "expanded_height",
    "position_x_offset",
    "position_y_offset",
    "dock_position",
    "dock_position_top_center",
    "dock_position_top_left",
    "dock_position_top_right",
    "dock_position_bottom_center",
    "dock_position_bottom_left",
    "dock_position_bottom_right",
    "monitor",
    "font_size",
    "motion_blur",
    "cover_rotate",
    "audio_gate",
    "section_experimental",
    "mini_controls",
    "font_preview_sample",
    "island_style",
    "style_default",
    "style_glass",
    "style_mica",
    "style_dynamic",
    "style_liquid_glass",
    "liquid_glass_blur",
    "liquid_glass_refraction",
    "liquid_glass_dispersion",
    "liquid_glass_tint",
    "liquid_glass_siri",
    "liquid_glass_siri_note",
    "glass_blur",
    "lyrics_transition_animation",
    "lyrics_transition_blur",
    "lyrics_transition_slide",
    "lyrics_transition_fade",
    "custom_font",
    "font_select",
    "font_reset",
    "font_preview_default",
    "font_preview_custom",
    "start_boot",
    "auto_hide",
    "check_updates",
    "update_interval",
    "language",
    "lang_name",
    "hide_delay",
    "hover_to_hide",
    "hover_to_hide_distance",
    "hover_to_hide_delay",
    "spectrum_wheel_volume",
    "non_expanded_wheel_volume",
    "reset_defaults",
    "visit_homepage",
    "created_by",
    "music_settings_title",
    "show_lyrics",
    "lyrics_ws_source",
    "lyrics_ws_address",
    "lyrics_delay",
    "lyrics_scroll",
    "lyrics_scroll_max_width",
    "lyrics_scroll_infinite_loop",
    "lyrics_filter_scope",
    "lyrics_filter_off",
    "lyrics_filter_desktop",
    "lyrics_filter_all",
    "lyrics_filter_regex",
    "lyrics_filter_regex_placeholder",
    "lyrics_filter_invalid_regex",
    "lyrics_char_highlight",
    "lyrics_char_lift_animation",
    "lyrics_char_color_unplayed",
    "lyrics_char_color_played",
    "lyrics_char_color_placeholder",
    "folder_select",
    "folder_clear",
    "delete",
    "section_process_rules",
    "process_name",
    "add_rule",
    "remove_rule",
    "update_available_title",
    "update_available_desc",
    "check_updates_now",
    "update_no_updates_title",
    "update_no_updates_desc",
    "update_check_failed_title",
    "update_check_failed_desc",
    "update_failed_title",
    "update_failed_dl",
    "update_failed_save",
    "tray_show",
    "tray_hide",
    "tray_settings",
    "tray_restart",
    "tray_exit",
    "action_copy",
    "action_paste",
];

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
fn save_settings_config(mut config: AppConfig) -> Result<SettingsState, String> {
    let old_config = load_config();
    normalize_config(&mut config);
    apply_config_side_effects(&old_config, &config);
    save_config(&config);
    Ok(build_state(config))
}

#[tauri::command]
fn reset_settings() -> Result<SettingsState, String> {
    let config = AppConfig::default();
    init_i18n(&config.language);
    FontManager::global().refresh_custom_font();
    save_config(&config);
    Ok(build_state(config))
}

#[tauri::command]
fn select_font_file() -> Result<SettingsState, String> {
    let mut config = load_config();
    if let Some(path) = rfd::FileDialog::new()
        .add_filter("Fonts", &["ttf", "otf"])
        .pick_file()
    {
        let path = path.to_string_lossy().into_owned();
        if !can_load_font_file(&path) {
            return Err("无法加载所选字体文件".to_string());
        }
        config.custom_font_path = Some(path);
        FontManager::global().refresh_custom_font();
        save_config(&config);
    }
    Ok(build_state(config))
}

#[tauri::command]
fn reset_font() -> Result<SettingsState, String> {
    let mut config = load_config();
    config.custom_font_path = None;
    FontManager::global().refresh_custom_font();
    save_config(&config);
    Ok(build_state(config))
}

#[tauri::command]
fn check_updates_now() -> Result<(), String> {
    crate::utils::updater::check_for_updates_now();
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
            watch_main_instance(app.handle().clone());
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

fn build_state(config: AppConfig) -> SettingsState {
    SettingsState {
        lyrics_filter_regex_valid: regex::Regex::new(&config.lyrics_filter_regex).is_ok(),
        config,
        current_lang: current_lang(),
        translations: translation_map(),
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

fn translation_map() -> HashMap<String, String> {
    TRANSLATION_KEYS
        .iter()
        .map(|key| ((*key).to_string(), tr(key)))
        .collect()
}

fn normalize_config(config: &mut AppConfig) {
    config.non_expanded_scale = round_to(config.non_expanded_scale, 0.01).clamp(0.5, 5.0);
    config.expanded_scale = round_to(config.expanded_scale, 0.01).clamp(0.5, 5.0);
    config.base_width = config.base_width.max(40.0);
    config.base_height = config.base_height.max(15.0);
    config.expanded_width = config.expanded_width.max(200.0);
    config.expanded_height = config.expanded_height.max(100.0);
    config.liquid_glass_blur = config.liquid_glass_blur.clamp(0.0, 30.0);
    config.liquid_glass_refraction = config.liquid_glass_refraction.clamp(0.0, 400.0);
    config.liquid_glass_dispersion = config.liquid_glass_dispersion.clamp(0.0, 100.0);
    config.liquid_glass_tint = config.liquid_glass_tint.clamp(0.0, 200.0);
    config.glass_blur = config.glass_blur.clamp(0.0, 50.0);
    if !matches!(
        config.lyrics_transition_animation.as_str(),
        "blur" | "slide" | "fade"
    ) {
        config.lyrics_transition_animation = AppConfig::default().lyrics_transition_animation;
    }
    config.font_size = config.font_size.clamp(0.0, 30.0);
    config.auto_hide_delay = config.auto_hide_delay.clamp(1.0, 60.0);
    config.hover_to_hide_distance = config.hover_to_hide_distance.clamp(50.0, 300.0);
    config.hover_to_hide_delay = round_to(config.hover_to_hide_delay, 0.1).clamp(0.2, 3.0);
    config.update_check_interval = config.update_check_interval.clamp(1.0, 24.0);
    config.lyrics_delay = round_to_f64(config.lyrics_delay, 0.1).clamp(-10.0, 10.0);
    if !config.spectrum_wheel_volume {
        config.non_expanded_wheel_volume = false;
    }
    let lyrics_scroll_min_width = (config.base_width + 35.0).max(100.0);
    config.lyrics_scroll_max_width = config.lyrics_scroll_max_width.clamp(
        lyrics_scroll_min_width,
        500.0_f32.max(lyrics_scroll_min_width),
    );

    if !matches!(config.language.as_str(), "auto" | "en" | "zh") {
        config.language = AppConfig::default().language;
    }
    if !matches!(config.settings_theme.as_str(), "system" | "light" | "dark") {
        config.settings_theme = AppConfig::default().settings_theme;
    }
    if !matches!(
        config.island_style.as_str(),
        "default" | "glass" | "mica" | "dynamic" | "liquid_glass"
    ) {
        config.island_style = AppConfig::default().island_style;
    }
    if !matches!(config.mini_cover_shape.as_str(), "square" | "circle") {
        config.mini_cover_shape = AppConfig::default().mini_cover_shape;
    }
    if !matches!(config.expanded_cover_shape.as_str(), "square" | "circle") {
        config.expanded_cover_shape = AppConfig::default().expanded_cover_shape;
    }

    if !is_valid_color(&config.lyrics_char_color_unplayed) {
        config.lyrics_char_color_unplayed = AppConfig::default().lyrics_char_color_unplayed;
    }
    if !is_valid_color(&config.lyrics_char_color_played) {
        config.lyrics_char_color_played = AppConfig::default().lyrics_char_color_played;
    }
}

fn round_to(value: f32, step: f32) -> f32 {
    (value / step).round() * step
}

fn round_to_f64(value: f64, step: f64) -> f64 {
    (value / step).round() * step
}

fn apply_config_side_effects(old_config: &AppConfig, config: &AppConfig) {
    if old_config.language != config.language {
        init_i18n(&config.language);
    }
    if old_config.auto_start != config.auto_start
        && let Err(err) = set_autostart(config.auto_start)
    {
        log::warn!("EchoMusic-Lyrics-WinIsland: failed to update autostart: {err}");
    }
    if old_config.custom_font_path != config.custom_font_path {
        FontManager::global().refresh_custom_font();
    }
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
                label = format!("Display {active_count}: {label}");
                monitors.push(label);
            }

            idx += 1;
        }
    }

    if monitors.is_empty() {
        monitors.push("Primary".to_string());
    }

    monitors
}
