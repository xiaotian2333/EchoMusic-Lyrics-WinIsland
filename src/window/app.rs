use std::sync::Arc;
use std::time::{Duration, Instant};

use regex::Regex;
use softbuffer::{Context, Surface};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows::Win32::UI::WindowsAndMessaging::{
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX, WS_THICKFRAME,
};
use windows::core::PCWSTR;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Window, WindowButtons, WindowId, WindowLevel};

use crate::core::audio::AudioProcessor;
use crate::core::config::{
    AppConfig, DockPosition, LyricsFilterScope, PADDING, TOP_OFFSET, WINDOW_TITLE,
};
use crate::core::lyrics::{
    LyricCharacter, current_character_index, current_lyric_index, filtered_lyric_text,
};
use crate::core::lyrics_ws::normalize_volume_value;
use crate::core::media_info::MediaInfo;
use crate::core::persistence::load_config;
use crate::core::render::{
    draw_island, draw_island_canvas, get_mini_control_rects, get_mini_visualizer_hit_rect,
};
use crate::core::ws_media::WsMediaListener;
use crate::ui::expanded::music_view::{
    get_expanded_visualizer_hit_rect, get_favorite_btn_rect, get_music_info_rect,
    get_next_btn_rect, get_pause_btn_rect, get_play_mode_btn_rect, get_prev_btn_rect,
    get_progress_bar_rect, set_progress_dragging, set_progress_hover, trigger_cover_flip,
    trigger_next_click, trigger_pause_click, trigger_prev_click,
};
use crate::utils::backdrop::{clear_mica_cache, disable_mica};
use crate::utils::blur::calculate_blur_sigmas;
use crate::utils::color::parse_hex_color;
use crate::utils::font::FontManager;
use crate::utils::icon::get_app_icon;
use crate::utils::liquid_glass::{clear_liquid_glass_cache, set_exclude_from_capture};
use crate::utils::mouse::{
    get_global_cursor_pos, is_cursor_hidden, is_foreground_fullscreen, is_left_button_pressed,
    is_point_in_rect,
};
use crate::utils::physics::Spring;
use crate::utils::process::get_foreground_process_name;
use crate::window::tray::{TrayAction, TrayManager};

const MINI_LYRIC_LOOP_GAP: f32 = 48.0;
/// GPU 渲染管线初始化失败后的重试间隔。
const GPU_RETRY_INTERVAL: Duration = Duration::from_secs(10);
/// 毛玻璃宿主背板初始化失败后的重试间隔。
const HOST_BACKDROP_RETRY_INTERVAL: Duration = Duration::from_secs(10);
/// 毛玻璃伴随窗口标题；仅用于调试识别，不显示在任务栏。
const BACKDROP_WINDOW_TITLE: &str = "EchoMusic-Lyrics-WinIsland Backdrop";
const VOLUME_WHEEL_SLOW_STEP: f32 = 2.0;
const VOLUME_WHEEL_MEDIUM_STEP: f32 = 4.0;
const VOLUME_WHEEL_FAST_STEP: f32 = 8.0;
const VOLUME_WHEEL_MAX_UNITS: f32 = 1.0;

pub struct App {
    window: Option<Arc<Window>>,
    context: Option<Context<Arc<Window>>>,
    surface: Option<Surface<Arc<Window>, Arc<Window>>>,
    tray: Option<TrayManager>,
    media: WsMediaListener,
    audio: AudioProcessor,
    config: AppConfig,
    expanded: bool,
    widget_view: bool,
    visible: bool,
    restart_requested: bool,
    spring_w: Spring,
    spring_h: Spring,
    spring_r: Spring,
    spring_view: Spring,
    os_w: u32,
    os_h: u32,
    win_x: i32,
    win_y: i32,
    frame_count: u64,
    last_media_title: String,
    last_media_thumbnail_hash: u64,
    last_media_playing: bool,
    current_lyric_text: String,
    old_lyric_text: String,
    lyric_transition: f32,
    lyrics_filter_pattern_cache: String,
    lyrics_filter_regex_cache: Option<Regex>,
    idle_timer: Instant,
    spring_hide: Spring,
    auto_hidden: bool,
    hover_hidden: bool,
    is_dragging: bool,
    drag_start_py: i32,
    drag_start_hide_val: f32,
    manually_hidden: bool,
    drag_has_moved: bool,
    last_frame_time: Instant,
    last_mon_size: (u32, u32),
    last_mon_pos: (i32, i32),
    lyric_scroll_active: bool,
    lyric_scroll_offset: f32,
    lyric_scroll_pause: f32,
    seeking_progress: bool,
    seeking_bar_left: f32,
    seeking_bar_right: f32,
    seeking_duration_ms: u64,
    seeking_preview_ms: u64,
    is_fullscreen_suppressed: bool,
    is_cursor_suppressed: bool,
    effective_dock_position: DockPosition,
    last_foreground_process: Option<String>,
    touch_id: Option<u64>,
    touch_pos: PhysicalPosition<f64>,
    hover_to_hide_enter_at: Option<Instant>,
    last_volume_wheel_at: Option<Instant>,
    pending_volume: Option<f32>,
    gpu_island: Option<crate::gpu::GpuIsland>,
    gpu_retry_at: Option<Instant>,
    backdrop_window: Option<Arc<Window>>,
    backdrop_hwnd_raw: Option<isize>,
    host_backdrop: Option<crate::gpu::HostBackdrop>,
    host_backdrop_retry_at: Option<Instant>,
    hwnd_raw: Option<isize>,
}

impl Default for App {
    fn default() -> Self {
        let config = load_config();
        Self {
            window: None,
            context: None,
            surface: None,
            tray: None,
            config: config.clone(),
            expanded: false,
            widget_view: false,
            visible: true,
            restart_requested: false,
            spring_w: Spring::new(config.base_width * config.non_expanded_scale),
            spring_h: Spring::new(config.base_height * config.non_expanded_scale),
            spring_r: Spring::new((config.base_height * config.non_expanded_scale) / 2.0),
            spring_view: Spring::new(0.0),
            media: WsMediaListener::new(),
            audio: AudioProcessor::new(),
            os_w: 0,
            os_h: 0,
            win_x: 0,
            win_y: 0,
            frame_count: 0,
            last_media_title: String::new(),
            last_media_thumbnail_hash: 0,
            last_media_playing: false,
            current_lyric_text: String::new(),
            old_lyric_text: String::new(),
            lyric_transition: 1.0,
            lyrics_filter_pattern_cache: String::new(),
            lyrics_filter_regex_cache: None,
            idle_timer: Instant::now(),
            spring_hide: Spring::new(0.0),
            auto_hidden: false,
            hover_hidden: false,
            is_dragging: false,
            drag_start_py: 0,
            drag_start_hide_val: 0.0,
            manually_hidden: false,
            drag_has_moved: false,
            last_frame_time: Instant::now(),
            last_mon_size: (0, 0),
            last_mon_pos: (0, 0),
            lyric_scroll_active: false,
            lyric_scroll_offset: 0.0,
            lyric_scroll_pause: 0.0,
            seeking_progress: false,
            seeking_bar_left: 0.0,
            seeking_bar_right: 0.0,
            seeking_duration_ms: 0,
            seeking_preview_ms: 0,
            is_fullscreen_suppressed: false,
            is_cursor_suppressed: false,
            effective_dock_position: config.dock_position,
            last_foreground_process: None,
            touch_id: None,
            touch_pos: PhysicalPosition::new(0.0, 0.0),
            hover_to_hide_enter_at: None,
            last_volume_wheel_at: None,
            pending_volume: None,
            gpu_island: None,
            gpu_retry_at: None,
            backdrop_window: None,
            backdrop_hwnd_raw: None,
            host_backdrop: None,
            host_backdrop_retry_at: None,
            hwnd_raw: None,
        }
    }
}

struct IslandLayout {
    dock_bottom: bool,
    offset_x: f64,
    island_y: f64,
    current_island_y: f64,
    hide_distance: f64,
    hidden_handle_y: f64,
    hidden_handle_h: f64,
}

struct MiniLyricCharacterData<'a> {
    characters: &'a [LyricCharacter],
    char_idx: usize,
    current_pos: u64,
}

fn volume_wheel_step(elapsed: Option<Duration>) -> f32 {
    match elapsed {
        Some(elapsed) if elapsed < Duration::from_millis(80) => VOLUME_WHEEL_FAST_STEP,
        Some(elapsed) if elapsed <= Duration::from_millis(180) => VOLUME_WHEEL_MEDIUM_STEP,
        _ => VOLUME_WHEEL_SLOW_STEP,
    }
}

fn wheel_delta_units(delta: MouseScrollDelta) -> f32 {
    let units = match delta {
        MouseScrollDelta::LineDelta(_, y) => y,
        MouseScrollDelta::PixelDelta(pos) => pos.y as f32 / 120.0,
    };
    units.clamp(-VOLUME_WHEEL_MAX_UNITS, VOLUME_WHEEL_MAX_UNITS)
}

impl App {
    pub fn take_restart_requested(&mut self) -> bool {
        std::mem::take(&mut self.restart_requested)
    }

    fn is_hidden(&self) -> bool {
        self.auto_hidden || self.hover_hidden || self.manually_hidden
    }

    fn update_dock_position_from_process(&mut self, window: &Window) {
        if self.config.process_dock_rules.is_empty() {
            if self.effective_dock_position != self.config.dock_position {
                self.effective_dock_position = self.config.dock_position;
                self.last_foreground_process = None;
                self.reposition_window(window);
            }
            return;
        }

        let proc_name = get_foreground_process_name();

        if proc_name == self.last_foreground_process {
            return;
        }
        self.last_foreground_process = proc_name.clone();

        let matched = proc_name.and_then(|ref name| {
            self.config
                .process_dock_rules
                .iter()
                .find(|rule| {
                    let process_name = rule.process_name.trim();
                    !process_name.is_empty() && process_name.eq_ignore_ascii_case(name)
                })
                .map(|rule| rule.dock_position)
        });

        let new_position = matched.unwrap_or(self.config.dock_position);

        if new_position != self.effective_dock_position {
            self.effective_dock_position = new_position;
            self.reposition_window(window);
        }
    }

    fn reposition_window(&mut self, window: &Window) {
        if let Some(monitor) = Self::get_target_monitor(window, self.config.monitor_index) {
            let mon_size = monitor.size();
            let mon_pos = monitor.position();
            if mon_size.width > 0 && mon_size.height > 0 {
                self.last_mon_size = (mon_size.width, mon_size.height);
                self.last_mon_pos = (mon_pos.x, mon_pos.y);
                (self.win_x, self.win_y) = self.compute_window_position(mon_pos, mon_size);
                window.set_outer_position(PhysicalPosition::new(self.win_x, self.win_y));
                window.request_redraw();
            }
        }
    }

    fn set_aumid() {
        let aumid = "EchoMusic.Lyrics.WinIsland";
        let wide: Vec<u16> = aumid.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: SetCurrentProcessExplicitAppUserModelID sets a process-wide string identifier.
        // The wide string is valid and null-terminated. Called once during init before any windows.
        unsafe {
            let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR::from_raw(wide.as_ptr()));
        }
    }

    fn refresh_lyrics_filter_regex(&mut self) {
        if self.lyrics_filter_pattern_cache == self.config.lyrics_filter_regex {
            return;
        }
        self.lyrics_filter_pattern_cache = self.config.lyrics_filter_regex.clone();
        self.lyrics_filter_regex_cache = if self.config.lyrics_filter_regex.trim().is_empty() {
            None
        } else {
            match Regex::new(&self.config.lyrics_filter_regex) {
                Ok(regex) => Some(regex),
                Err(err) => {
                    log::warn!("歌词过滤正则无效: {}", err);
                    None
                }
            }
        };
    }

    fn current_filtered_lyric(&mut self, media: &MediaInfo) -> Option<String> {
        let lyrics = media.lyrics.as_ref()?;
        let raw_pos = if media.is_playing {
            media
                .position_ms
                .saturating_add(media.last_update.elapsed().as_millis() as u64)
        } else {
            media.position_ms
        };
        let current_pos =
            (raw_pos as i64 + (self.config.lyrics_delay * 1000.0) as i64).max(0) as u64;
        let idx = current_lyric_index(lyrics, current_pos)?;
        self.refresh_lyrics_filter_regex();
        if let Some(regex) = &self.lyrics_filter_regex_cache {
            Some(filtered_lyric_text(lyrics, idx, &media.title, |text| {
                regex.is_match(text)
            }))
        } else {
            Some(lyrics[idx].text.clone())
        }
    }

    fn get_target_monitor(
        window: &Window,
        monitor_index: i32,
    ) -> Option<winit::monitor::MonitorHandle> {
        if monitor_index < 0 {
            return window
                .primary_monitor()
                .or_else(|| window.current_monitor());
        }
        use windows::Win32::Graphics::Gdi::*;
        let mut win32_names: Vec<String> = Vec::new();
        // SAFETY: EnumDisplayDevicesW reads display device info. We provide a zeroed
        // DISPLAY_DEVICEW with correct cb size. idx increments safely. No mutable global state.
        unsafe {
            let mut idx = 0u32;
            loop {
                let mut dd: DISPLAY_DEVICEW = std::mem::zeroed();
                dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
                if EnumDisplayDevicesW(None, idx, &mut dd, 0).as_bool() {
                    if (dd.StateFlags & DISPLAY_DEVICE_ACTIVE) != 0 {
                        let name = String::from_utf16_lossy(&dd.DeviceName)
                            .trim_end_matches('\0')
                            .to_string();
                        win32_names.push(name);
                    }
                    idx += 1;
                } else {
                    break;
                }
            }
        }
        let target_name = win32_names.get(monitor_index as usize);
        let monitors: Vec<_> = window.available_monitors().collect();
        if let Some(name) = target_name {
            for mon in &monitors {
                if let Some(mon_name) = mon.name()
                    && (mon_name.contains(name.trim_start_matches("\\\\.\\"))
                        || name.contains(&mon_name))
                {
                    return Some(mon.clone());
                }
            }
        }
        let idx = monitor_index as usize;
        if idx < monitors.len() {
            monitors.get(idx).cloned()
        } else {
            window
                .primary_monitor()
                .or_else(|| window.current_monitor())
        }
    }

    fn enforce_topmost(window: &Window, win_x: i32, win_y: i32, os_w: u32, os_h: u32) {
        if let Ok(handle) = window.window_handle()
            && let RawWindowHandle::Win32(raw) = handle.as_raw()
        {
            let hwnd = HWND(raw.hwnd.get() as *mut core::ffi::c_void);
            crate::utils::win32::set_window_topmost(hwnd, win_x, win_y, os_w as i32, os_h as i32);
        }
    }

    fn compute_os_size(config: &AppConfig) -> (u32, u32) {
        let expanded_w = config.expanded_width.max(450.0) * config.expanded_scale;
        let expanded_h = config.expanded_height * config.expanded_scale;
        let non_expanded_w = config
            .base_width
            .max(config.base_width + 35.0)
            .max(config.lyrics_scroll_max_width)
            .max(450.0)
            * config.non_expanded_scale;
        let non_expanded_h =
            (config.base_height * config.non_expanded_scale).max(24.0 * config.non_expanded_scale);

        (
            (expanded_w.max(non_expanded_w) + PADDING).ceil() as u32,
            (expanded_h.max(non_expanded_h) + PADDING).ceil() as u32,
        )
    }

    fn compute_window_position(
        &self,
        mon_pos: PhysicalPosition<i32>,
        mon_size: PhysicalSize<u32>,
    ) -> (i32, i32) {
        let center_x = mon_pos.x + (mon_size.width as i32) / 2;
        let top_y = mon_pos.y + TOP_OFFSET;
        let bottom_y = mon_pos.y + mon_size.height as i32 - TOP_OFFSET;

        let win_x = if self.effective_dock_position.is_left() {
            mon_pos.x - (PADDING / 2.0) as i32 + TOP_OFFSET + self.config.position_x_offset
        } else if self.effective_dock_position.is_right() {
            mon_pos.x + mon_size.width as i32 - self.os_w as i32 + (PADDING / 2.0) as i32
                - TOP_OFFSET
                + self.config.position_x_offset
        } else {
            center_x - (self.os_w as i32) / 2 + self.config.position_x_offset
        };

        let win_y = if self.effective_dock_position.is_bottom() {
            bottom_y - self.os_h as i32 + (PADDING / 2.0) as i32 + self.config.position_y_offset
        } else {
            top_y - (PADDING / 2.0) as i32 + self.config.position_y_offset
        };

        (win_x, win_y)
    }

    fn compute_island_layout(&self) -> IslandLayout {
        let dock_bottom = self.effective_dock_position.is_bottom();
        let island_y = if dock_bottom {
            self.os_h as f64 - PADDING as f64 / 2.0 - self.spring_h.value as f64
        } else {
            PADDING as f64 / 2.0
        };

        let offset_x = if self.effective_dock_position.is_left() {
            PADDING as f64 / 2.0
        } else if self.effective_dock_position.is_right() {
            (self.os_w as f64 - PADDING as f64 / 2.0 - self.spring_w.value as f64).max(0.0)
        } else {
            (self.os_w as f64 - self.spring_w.value as f64) / 2.0
        };

        let scale = self.config.non_expanded_scale as f64;
        let hidden_peek_h = (5.0 * scale).max(3.0);
        let hide_distance = if dock_bottom {
            (self.spring_h.value as f64 - hidden_peek_h).max(0.0)
        } else {
            (self.spring_h.value as f64 - hidden_peek_h + TOP_OFFSET as f64).max(0.0)
        };
        let hide_y_offset = self.spring_hide.value as f64 * hide_distance;
        let current_island_y = if dock_bottom {
            island_y + hide_y_offset
        } else {
            island_y - hide_y_offset
        };

        let hidden_handle_h = (24.0 * scale).max(14.0);
        let hidden_handle_y = if dock_bottom {
            (self.os_h as f64 - PADDING as f64 / 2.0 - hidden_handle_h).max(0.0)
        } else {
            (current_island_y + self.spring_h.value as f64 - hidden_peek_h - hidden_handle_h * 0.35)
                .max(0.0)
        };

        IslandLayout {
            dock_bottom,
            offset_x,
            island_y,
            current_island_y,
            hide_distance,
            hidden_handle_y,
            hidden_handle_h,
        }
    }

    fn expansion_progress(&self) -> f32 {
        let collapsed_h = self.config.base_height * self.config.non_expanded_scale;
        let expanded_h = self.config.expanded_height * self.config.expanded_scale;
        let delta_h = expanded_h - collapsed_h;
        if delta_h.abs() < 0.001 {
            if self.expanded { 1.0 } else { 0.0 }
        } else {
            ((self.spring_h.value - collapsed_h) / delta_h).clamp(0.0, 1.0)
        }
    }

    fn cursor_on_volume_wheel_area(&self, px: i32, py: i32, media: &MediaInfo) -> bool {
        if media.title.is_empty() || self.is_hidden() {
            return false;
        }

        let rel_x = px - self.win_x;
        let rel_y = py - self.win_y;
        let layout = self.compute_island_layout();
        let progress = self.expansion_progress();

        if self.expanded {
            if self.spring_view.value >= 0.5 {
                return false;
            }
            let (x, y, w, h) = get_expanded_visualizer_hit_rect(
                layout.offset_x as f32,
                layout.current_island_y as f32,
                self.spring_w.value,
                self.config.expanded_scale,
                progress,
                &self.config.expanded_cover_shape,
            );
            let page_shift = self.spring_view.value * self.spring_w.value;
            let cx = rel_x as f32 + page_shift;
            let cy = rel_y as f32;
            cx >= x && cx <= x + w && cy >= y && cy <= y + h
        } else {
            if self.config.non_expanded_wheel_volume {
                return is_point_in_rect(
                    rel_x as f64,
                    rel_y as f64,
                    layout.offset_x,
                    layout.current_island_y,
                    self.spring_w.value as f64,
                    self.spring_h.value as f64,
                );
            }
            if self.spring_w.value <= 45.0 * self.config.non_expanded_scale {
                return false;
            }
            let (x, y, w, h) = get_mini_visualizer_hit_rect(
                layout.offset_x as f32,
                layout.current_island_y as f32,
                self.spring_w.value,
                self.spring_h.value,
                self.config.non_expanded_scale,
                progress,
            );
            let cx = rel_x as f32;
            let cy = rel_y as f32;
            cx >= x && cx <= x + w && cy >= y && cy <= y + h
        }
    }

    fn handle_volume_wheel(&mut self, delta: MouseScrollDelta, px: i32, py: i32) -> bool {
        if !self.config.spectrum_wheel_volume {
            return false;
        }

        let units = wheel_delta_units(delta);
        if units.abs() < 0.01 {
            return false;
        }

        let media = self.media.get_info();
        if !self.cursor_on_volume_wheel_area(px, py, &media) {
            return false;
        }

        let now = Instant::now();
        let elapsed = self
            .last_volume_wheel_at
            .map(|last| now.saturating_duration_since(last));
        let reset_base = match elapsed {
            Some(elapsed) => elapsed > Duration::from_secs(1),
            None => true,
        };
        let base_volume = if reset_base {
            media.volume.unwrap_or(50.0)
        } else {
            self.pending_volume.or(media.volume).unwrap_or(50.0)
        };
        let step = volume_wheel_step(elapsed);
        let next_volume =
            normalize_volume_value((base_volume + step * units) as f64).unwrap_or(base_volume);

        self.last_volume_wheel_at = Some(now);
        self.pending_volume = Some(next_volume);
        self.media.request_set_volume(next_volume);
        self.idle_timer = now;
        true
    }

    fn measure_lyric_text_width(&self, text: &str) -> f32 {
        let mut text_w: f32 = 0.0;
        for c in text.chars() {
            if c.is_ascii() {
                text_w += 7.5;
            } else {
                text_w += 13.5;
            }
        }
        text_w
    }

    fn mini_lyric_font_size(&self) -> f32 {
        if self.config.font_size > 0.0 {
            self.config.font_size * 0.8 * self.config.non_expanded_scale
        } else {
            12.0 * self.config.non_expanded_scale
        }
    }

    fn measure_mini_lyric_render_width(&self, text: &str) -> f32 {
        FontManager::global().measure_text_cached(
            text,
            self.mini_lyric_font_size(),
            skia_safe::FontStyle::normal(),
        )
    }

    fn measure_mini_lyric_characters_width(&self, characters: &[LyricCharacter]) -> f32 {
        characters
            .iter()
            .map(|c| {
                FontManager::global().measure_text_cached(
                    &c.t,
                    self.mini_lyric_font_size(),
                    skia_safe::FontStyle::normal(),
                )
            })
            .sum()
    }

    fn mini_lyric_loop_gap(&self) -> f32 {
        MINI_LYRIC_LOOP_GAP * self.config.non_expanded_scale
    }

    fn mini_lyric_character_scroll_offset(
        &self,
        data: &MiniLyricCharacterData<'_>,
        available_text_w: f32,
        overflow: f32,
    ) -> f32 {
        if overflow <= 0.0 {
            return 0.0;
        }

        let mut play_x = 0.0;
        for (idx, ch) in data.characters.iter().enumerate() {
            let ch_w = FontManager::global().measure_text_cached(
                &ch.t,
                self.mini_lyric_font_size(),
                skia_safe::FontStyle::normal(),
            );
            if idx < data.char_idx {
                play_x += ch_w;
                continue;
            }
            if idx == data.char_idx {
                let progress = if ch.e > ch.s {
                    ((data.current_pos.saturating_sub(ch.s)) as f32 / (ch.e - ch.s) as f32)
                        .clamp(0.0, 1.0)
                } else {
                    0.5
                };
                play_x += ch_w * progress;
                break;
            }
        }

        (play_x - available_text_w / 2.0).clamp(0.0, overflow)
    }

    fn current_desktop_character_data<'a>(
        &mut self,
        media: &'a MediaInfo,
    ) -> Option<MiniLyricCharacterData<'a>> {
        if !self.config.show_lyrics || !self.config.lyrics_char_highlight {
            return None;
        }
        let lyrics = media.lyrics.as_ref()?;
        let delay_ms = (self.config.lyrics_delay * 1000.0) as i64;
        let current_pos = media.effective_position_ms(delay_ms);
        let idx = current_lyric_index(lyrics, current_pos)?;
        let line = &lyrics[idx];
        let characters = line.characters.as_ref()?;
        if characters.is_empty() {
            return None;
        }
        if self.config.lyrics_filter_scope.filters_desktop() {
            self.refresh_lyrics_filter_regex();
            if self
                .lyrics_filter_regex_cache
                .as_ref()
                .is_some_and(|regex| regex.is_match(line.text.trim()))
            {
                return None;
            }
        }
        let char_idx = current_character_index(characters, current_pos)?;
        Some(MiniLyricCharacterData {
            characters,
            char_idx,
            current_pos,
        })
    }

    fn handle_input(&mut self, state: ElementState, px: i32, py: i32) {
        if self.is_fullscreen_suppressed || self.is_cursor_suppressed {
            return;
        }
        let rel_x = px - self.win_x;
        let rel_y = py - self.win_y;
        let layout = self.compute_island_layout();

        if state == ElementState::Pressed {
            self.handle_press(rel_x, rel_y, &layout);
        } else if state == ElementState::Released {
            self.handle_release(py);
        }
    }

    fn handle_press(&mut self, rel_x: i32, rel_y: i32, layout: &IslandLayout) {
        let island_y = layout.island_y;
        let offset_x = layout.offset_x;
        let current_island_y = layout.current_island_y;

        let is_hovering_visible = is_point_in_rect(
            rel_x as f64,
            rel_y as f64,
            offset_x,
            current_island_y,
            self.spring_w.value as f64,
            self.spring_h.value as f64,
        );

        let hidden_handle_h = layout.hidden_handle_h;
        let hidden_handle_y = layout.hidden_handle_y;
        let is_on_hidden_handle = self.is_hidden()
            && is_point_in_rect(
                rel_x as f64,
                rel_y as f64,
                offset_x,
                hidden_handle_y,
                self.spring_w.value as f64,
                hidden_handle_h,
            );

        if self.expanded {
            let view_val = self.spring_view.value as f64;
            let w = self.spring_w.value as f64;
            let h = self.spring_h.value as f64;
            let page_shift = view_val * w;
            let scale = self.config.expanded_scale as f64;

            if view_val < 0.5 {
                let media = self.media.get_info();
                let music_on = !media.title.is_empty();

                let (bx, by, bw, bh) = get_pause_btn_rect(
                    offset_x as f32,
                    island_y as f32,
                    w as f32,
                    h as f32,
                    self.config.expanded_scale,
                    &self.config.expanded_cover_shape,
                );
                let cx = rel_x as f32 - (page_shift as f32);
                let cy = rel_y as f32;
                if music_on && cx >= bx && cx <= bx + bw && cy >= by && cy <= by + bh {
                    trigger_pause_click(media.is_playing);
                    self.media.request_toggle_play();
                    return;
                }

                let (px, py, pw, ph) = get_prev_btn_rect(
                    offset_x as f32,
                    island_y as f32,
                    w as f32,
                    h as f32,
                    self.config.expanded_scale,
                    &self.config.expanded_cover_shape,
                );
                if music_on && cx >= px && cx <= px + pw && cy >= py && cy <= py + ph {
                    trigger_cover_flip();
                    trigger_prev_click();
                    self.media.request_prev();
                    return;
                }

                let (nx, ny, nw, nh) = get_next_btn_rect(
                    offset_x as f32,
                    island_y as f32,
                    w as f32,
                    h as f32,
                    self.config.expanded_scale,
                    &self.config.expanded_cover_shape,
                );
                if music_on && cx >= nx && cx <= nx + nw && cy >= ny && cy <= ny + nh {
                    trigger_cover_flip();
                    trigger_next_click();
                    self.media.request_next();
                    return;
                }

                let (pmx, pmy, pmw, pmh) = get_play_mode_btn_rect(
                    offset_x as f32,
                    island_y as f32,
                    w as f32,
                    h as f32,
                    self.config.expanded_scale,
                    &self.config.expanded_cover_shape,
                );
                if music_on && cx >= pmx && cx <= pmx + pmw && cy >= pmy && cy <= pmy + pmh {
                    let next_mode = match media.play_mode.as_str() {
                        "sequential" => "list",
                        "list" => "random",
                        "random" => "single",
                        "single" => "sequential",
                        _ => "list",
                    };
                    self.media.request_set_play_mode(next_mode.to_string());
                    return;
                }

                let (fx, fy, fw, fh) = get_favorite_btn_rect(
                    offset_x as f32,
                    island_y as f32,
                    w as f32,
                    h as f32,
                    self.config.expanded_scale,
                    &self.config.expanded_cover_shape,
                );
                if music_on && cx >= fx && cx <= fx + fw && cy >= fy && cy <= fy + fh {
                    self.media
                        .request_set_favorite(!media.is_favorite, media.track_id.clone());
                    return;
                }

                if let Some((bar_left, bar_right, bar_top, bar_hit_h)) = get_progress_bar_rect(
                    offset_x as f32,
                    island_y as f32,
                    w as f32,
                    &media,
                    music_on,
                    self.config.expanded_scale,
                    &self.config.expanded_cover_shape,
                ) && cx >= bar_left
                    && cx <= bar_right
                    && cy >= bar_top
                    && cy <= bar_top + bar_hit_h
                {
                    let ratio = ((cx - bar_left) / (bar_right - bar_left)).clamp(0.0, 1.0);
                    let duration_ms = media.effective_duration_ms();
                    let seek_ms = (ratio as f64 * duration_ms as f64) as u64;
                    self.seeking_progress = true;
                    self.seeking_bar_left = bar_left;
                    self.seeking_bar_right = bar_right;
                    self.seeking_duration_ms = duration_ms;
                    self.seeking_preview_ms = seek_ms;
                    return;
                }

                let (ix, iy, iw, ih) = get_music_info_rect(
                    offset_x as f32,
                    island_y as f32,
                    w as f32,
                    self.config.expanded_scale,
                    &self.config.expanded_cover_shape,
                );
                if cx >= ix && cx <= ix + iw && cy >= iy && cy <= iy + ih {
                    self.media.request_show_main_window();
                    return;
                }
            }

            if view_val > 0.5 {
                let gear_x = offset_x + w - 28.0 * scale + w - page_shift;
                let gear_y = island_y + h - 28.0 * scale;
                let dist_sq = (rel_x as f64 - gear_x).powi(2) + (rel_y as f64 - gear_y).powi(2);
                if dist_sq <= (20.0 * scale).powi(2) {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe).arg("--settings").spawn();
                    }
                    return;
                }

                let arrow_x = offset_x + 12.0 * scale + w - page_shift;
                let arrow_y = island_y + h / 2.0;
                let adx = rel_x as f64 - arrow_x;
                let ady = rel_y as f64 - arrow_y;
                if adx * adx + ady * ady <= (20.0 * scale).powi(2) {
                    self.widget_view = false;
                    return;
                }
            }

            if view_val < 0.5 {
                let arrow_x = offset_x + w - 12.0 * scale;
                let arrow_y = island_y + h / 2.0;
                let adx = rel_x as f64 - arrow_x;
                let ady = rel_y as f64 - arrow_y;
                if adx * adx + ady * ady <= (20.0 * scale).powi(2) {
                    self.widget_view = true;
                    return;
                }
            }

            if (rel_y as f64) < island_y + 40.0 * scale {
                self.expanded = false;
                self.widget_view = false;
            }
        } else {
            let media = self.media.get_info();
            let music_on = !media.title.is_empty();

            if music_on && !media.is_playing && self.config.mini_controls {
                let w = self.spring_w.value;
                let h = self.spring_h.value;
                let (prev_rect, play_rect, next_rect) = get_mini_control_rects(
                    offset_x as f32,
                    current_island_y as f32,
                    w,
                    h,
                    self.config.non_expanded_scale,
                );

                let cx = rel_x as f32;
                let cy = rel_y as f32;

                let mut hit_control = false;
                if let Some((px, py, pw, ph)) = prev_rect
                    && cx >= px
                    && cx <= px + pw
                    && cy >= py
                    && cy <= py + ph
                {
                    self.media.request_prev();
                    hit_control = true;
                }
                if !hit_control
                    && let Some((px, py, pw, ph)) = play_rect
                    && cx >= px
                    && cx <= px + pw
                    && cy >= py
                    && cy <= py + ph
                {
                    self.media.request_toggle_play();
                    hit_control = true;
                }
                if !hit_control
                    && let Some((px, py, pw, ph)) = next_rect
                    && cx >= px
                    && cx <= px + pw
                    && cy >= py
                    && cy <= py + ph
                {
                    self.media.request_next();
                    hit_control = true;
                }
                if hit_control {
                    return;
                }
            }

            if is_hovering_visible || is_on_hidden_handle {
                self.is_dragging = true;
                self.drag_start_py = rel_y + self.win_y;
                self.drag_start_hide_val = self.spring_hide.value;
                self.drag_has_moved = false;
            }
        }
    }

    fn handle_release(&mut self, _py: i32) {
        if self.seeking_progress {
            self.seeking_progress = false;
            if self.seeking_duration_ms > 0 {
                self.media.request_seek(self.seeking_preview_ms);
            }
            return;
        }
        if self.is_dragging {
            self.is_dragging = false;
            if !self.drag_has_moved {
                if self.is_hidden() {
                    self.auto_hidden = false;
                    self.hover_hidden = false;
                    self.manually_hidden = false;
                    self.hover_to_hide_enter_at = None;
                    self.spring_hide.velocity = -0.45;
                    self.idle_timer = Instant::now();
                } else {
                    self.expanded = true;
                }
            } else if self.spring_hide.value > 0.3 {
                self.manually_hidden = true;
                self.auto_hidden = false;
                self.hover_hidden = false;
                self.hover_to_hide_enter_at = None;
            } else {
                self.manually_hidden = false;
                self.auto_hidden = false;
                self.hover_hidden = false;
                self.hover_to_hide_enter_at = None;
            }
        }
    }

    fn close_settings_window() {
        crate::utils::win32::close_window("EchoMusic-Lyrics-WinIsland Settings");
    }

    fn handle_tray_events(&mut self, window: &Window, event_loop: &ActiveEventLoop) {
        if let Some(tray) = &self.tray
            && let Some(action) = tray.try_recv_action()
        {
            match action {
                TrayAction::ToggleVisibility => {
                    self.visible = !self.visible;
                    window.set_visible(self.visible);
                    tray.update_item_text(self.visible);
                    if !self.visible {
                        // 主岛隐藏时同步收起毛玻璃背板，避免伴随窗口留在屏幕上
                        self.update_host_backdrop(None, 0.0);
                    }
                }
                TrayAction::OpenSettings => {
                    if let Ok(exe) = std::env::current_exe() {
                        let _ = std::process::Command::new(exe).arg("--settings").spawn();
                    }
                }
                TrayAction::Restart => {
                    Self::close_settings_window();
                    self.restart_requested = true;
                    event_loop.exit();
                }
                TrayAction::Exit => {
                    Self::close_settings_window();
                    event_loop.exit();
                }
            }
        }
    }

    /// 按当前样式维护 GPU 渲染管线：liquid_glass 时惰性创建，其他样式释放。
    fn ensure_gpu_island(&mut self) {
        if self.config.island_style != "liquid_glass" {
            if self.gpu_island.is_some() {
                // Drop 会分离 DirectComposition 视觉树并关闭捕获会话
                self.gpu_island = None;
            }
            return;
        }
        if self.gpu_island.is_some() {
            return;
        }
        if let Some(retry_at) = self.gpu_retry_at
            && retry_at.elapsed() < GPU_RETRY_INTERVAL
        {
            return;
        }
        let Some(hwnd_raw) = self.hwnd_raw else {
            return;
        };
        if self.os_w == 0 || self.os_h == 0 {
            return;
        }
        match crate::gpu::GpuIsland::new(hwnd_raw, self.os_w, self.os_h) {
            Ok(gpu) => {
                self.gpu_island = Some(gpu);
                self.gpu_retry_at = None;
                // GPU 模式由合成视觉树呈现；清空软缓冲，避免旧 CPU 画面从透明区域透出
                if let Some(surface) = self.surface.as_mut()
                    && let Ok(mut buffer) = surface.buffer_mut()
                {
                    for p in buffer.iter_mut() {
                        *p = 0;
                    }
                    let _ = buffer.present();
                }
            }
            Err(error) => {
                log::warn!("GPU 渲染管线初始化失败，使用 CPU 液态玻璃路径: {error}");
                self.gpu_retry_at = Some(Instant::now());
            }
        }
    }

    /// GPU 渲染失败时释放管线并进入冷却，下一帧回退到 CPU 路径。
    fn fail_gpu_island(&mut self, error: &str) {
        log::warn!("GPU 渲染失败，回退到 CPU 液态玻璃路径: {error}");
        self.gpu_island = None;
        self.gpu_retry_at = Some(Instant::now());
    }

    /// 按当前样式维护毛玻璃宿主背板：glass 时惰性创建，其他样式释放。
    fn ensure_host_backdrop(&mut self) {
        if self.config.island_style != "glass" {
            // Drop 会隐藏伴随窗口并关闭合成目标
            self.host_backdrop = None;
            return;
        }
        if self.host_backdrop.is_some() {
            return;
        }
        if let Some(retry_at) = self.host_backdrop_retry_at
            && retry_at.elapsed() < HOST_BACKDROP_RETRY_INTERVAL
        {
            return;
        }
        let (Some(main_hwnd), Some(backdrop_hwnd)) = (self.hwnd_raw, self.backdrop_hwnd_raw) else {
            return;
        };
        match crate::gpu::HostBackdrop::new(main_hwnd, backdrop_hwnd) {
            Ok(host_backdrop) => {
                self.host_backdrop = Some(host_backdrop);
                self.host_backdrop_retry_at = None;
            }
            Err(error) => {
                log::warn!("毛玻璃宿主背板初始化失败，使用 CPU 截屏毛玻璃路径: {error}");
                self.host_backdrop_retry_at = Some(Instant::now());
            }
        }
    }

    /// 释放宿主背板并进入冷却，下一帧回退到 CPU 截屏毛玻璃。
    fn fail_host_backdrop(&mut self, error: &str) {
        log::warn!("毛玻璃宿主背板更新失败，回退到 CPU 截屏毛玻璃路径: {error}");
        self.host_backdrop = None;
        self.host_backdrop_retry_at = Some(Instant::now());
    }

    /// 同步宿主背板形状与磨砂程度；`shape` 为 None 时隐藏背板窗口。
    fn update_host_backdrop(&mut self, shape: Option<crate::gpu::HostBackdropShape>, blur: f32) {
        let Some(host_backdrop) = self.host_backdrop.as_ref() else {
            return;
        };
        if let Err(error) = host_backdrop.update(shape, blur) {
            self.fail_host_backdrop(&error);
        }
    }

    /// 岛屿在屏幕上的矩形与磨砂程度；`hide_progress` 会把矩形一起推出屏幕。
    fn host_backdrop_frame(&self, progress: f32) -> (Option<crate::gpu::HostBackdropShape>, f32) {
        if !self.visible {
            return (None, 0.0);
        }
        let layout = crate::core::render::LayoutParams {
            current_w: self.spring_w.value,
            current_h: self.spring_h.value,
            current_r: self.spring_r.value,
            os_w: self.os_w,
            os_h: self.os_h,
            sigmas: (0.0, 0.0),
            expansion_progress: progress,
            view_offset: self.spring_view.value,
            non_expanded_scale: self.config.non_expanded_scale,
            expanded_scale: self.config.expanded_scale,
            hide_progress: self.spring_hide.value,
            dock_position: self.effective_dock_position,
        };
        let (offset_x, offset_y) = crate::core::render::island_offset(&layout);
        let scale = self.config.non_expanded_scale
            + (self.config.expanded_scale - self.config.non_expanded_scale) * progress;
        (
            Some(crate::gpu::HostBackdropShape {
                screen_x: self.win_x as f32 + offset_x,
                screen_y: self.win_y as f32 + offset_y,
                width: self.spring_w.value,
                height: self.spring_h.value,
                radius: self.spring_r.value,
            }),
            self.config.glass_blur * scale,
        )
    }

    fn reload_config_if_changed(&mut self, window: &Window) {
        if !self.frame_count.is_multiple_of(30) {
            return;
        }
        let current_config = load_config();
        if current_config != self.config {
            let old_non_expanded_scale = self.config.non_expanded_scale;
            let old_expanded_scale = self.config.expanded_scale;
            let old_base_w = self.config.base_width;
            let old_base_h = self.config.base_height;
            let old_max_w = self.config.expanded_width;
            let old_max_h = self.config.expanded_height;
            let old_lyrics_scroll_max_width = self.config.lyrics_scroll_max_width;
            let old_style = self.config.island_style.clone();
            let old_mini_shape = self.config.mini_cover_shape.clone();
            let old_expanded_shape = self.config.expanded_cover_shape.clone();
            let old_font = self.config.custom_font_path.clone();

            self.config = current_config;
            self.last_foreground_process = None;
            self.effective_dock_position = self.config.dock_position;
            if old_style != self.config.island_style {
                crate::utils::backdrop::clear_dynamic_bg_cache();
                clear_mica_cache();
                clear_liquid_glass_cache();
                if let Ok(handle) = window.window_handle() {
                    let raw = handle.as_raw();
                    if let RawWindowHandle::Win32(win32_handle) = raw {
                        let hwnd = HWND(win32_handle.hwnd.get() as _);
                        if old_style == "mica" {
                            disable_mica(hwnd);
                        }
                        if old_style == "liquid_glass" {
                            set_exclude_from_capture(hwnd, false);
                        }
                        if self.config.island_style == "liquid_glass" {
                            set_exclude_from_capture(hwnd, true);
                        }
                    }
                }
                // 样式切换时释放 GPU 管线（Drop 会分离合成视觉树并关闭捕获会话）；
                // 进入液态玻璃时清除冷却，下一帧立即尝试重建
                self.gpu_island = None;
                self.gpu_retry_at = None;
                // 毛玻璃宿主背板同理：Drop 会隐藏伴随窗口并关闭合成目标
                self.host_backdrop = None;
                self.host_backdrop_retry_at = None;
            }

            if old_mini_shape != self.config.mini_cover_shape
                || old_expanded_shape != self.config.expanded_cover_shape
            {
                crate::ui::expanded::music_view::clear_cover_cache();
            }

            if old_font != self.config.custom_font_path {
                crate::utils::font::FontManager::global().refresh_custom_font();
            }

            self.media.broadcast_config_snapshot();

            let (new_os_w, new_os_h) = Self::compute_os_size(&self.config);

            let size_changed = new_os_w != self.os_w
                || new_os_h != self.os_h
                || (old_non_expanded_scale - self.config.non_expanded_scale).abs() > 0.001
                || (old_expanded_scale - self.config.expanded_scale).abs() > 0.001
                || (old_base_w - self.config.base_width).abs() > 0.1
                || (old_base_h - self.config.base_height).abs() > 0.1
                || (old_max_w - self.config.expanded_width).abs() > 0.1
                || (old_max_h - self.config.expanded_height).abs() > 0.1
                || (old_lyrics_scroll_max_width - self.config.lyrics_scroll_max_width).abs() > 0.1;

            if size_changed {
                self.os_w = new_os_w;
                self.os_h = new_os_h;
                let _ = window.request_inner_size(PhysicalSize::new(self.os_w, self.os_h));
                if let Some(surface) = self.surface.as_mut() {
                    let _ = surface.resize(
                        std::num::NonZeroU32::new(self.os_w.max(1)).unwrap(),
                        std::num::NonZeroU32::new(self.os_h.max(1)).unwrap(),
                    );
                }
                let (gpu_w, gpu_h) = (self.os_w, self.os_h);
                let gpu_resize = self.gpu_island.as_mut().map(|gpu| gpu.resize(gpu_w, gpu_h));
                if let Some(Err(error)) = gpu_resize {
                    self.fail_gpu_island(&error);
                }
            }

            if let Some(monitor) = Self::get_target_monitor(window, self.config.monitor_index) {
                let mon_size = monitor.size();
                let mon_pos = monitor.position();
                if mon_size.width > 0 && mon_size.height > 0 {
                    self.last_mon_size = (mon_size.width, mon_size.height);
                    self.last_mon_pos = (mon_pos.x, mon_pos.y);
                    (self.win_x, self.win_y) = self.compute_window_position(mon_pos, mon_size);
                    window.set_outer_position(PhysicalPosition::new(self.win_x, self.win_y));
                }
            }
        } else if let Some(monitor) = Self::get_target_monitor(window, self.config.monitor_index) {
            let mon_size = monitor.size();
            let mon_pos = monitor.position();
            let cur_mon_size = (mon_size.width, mon_size.height);
            let cur_mon_pos = (mon_pos.x, mon_pos.y);
            if (cur_mon_size != self.last_mon_size || cur_mon_pos != self.last_mon_pos)
                && cur_mon_size.0 > 0
                && cur_mon_size.1 > 0
            {
                self.last_mon_size = cur_mon_size;
                self.last_mon_pos = cur_mon_pos;
                (self.win_x, self.win_y) = self.compute_window_position(mon_pos, mon_size);
                window.set_outer_position(PhysicalPosition::new(self.win_x, self.win_y));
            }
        }
    }

    fn compute_lyric_target_width(
        &mut self,
        window: &Window,
        music_active: bool,
        is_paused: bool,
        lyric_character_data: Option<&MiniLyricCharacterData<'_>>,
        dt: f32,
    ) -> f32 {
        self.lyric_scroll_active = false;
        let is_currently_hidden = self.is_hidden() || self.spring_hide.value > 0.1;
        let target_base_w = if music_active && !self.expanded && !is_currently_hidden {
            let has_visible_lyrics = !self.current_lyric_text.is_empty()
                || (!self.old_lyric_text.is_empty() && self.lyric_transition < 1.0);

            if has_visible_lyrics {
                if self.config.lyrics_scroll {
                    let display_text = if !self.current_lyric_text.is_empty() {
                        &self.current_lyric_text
                    } else {
                        &self.old_lyric_text
                    };
                    let character_text_w = lyric_character_data
                        .map(|data| self.measure_mini_lyric_characters_width(data.characters));
                    let text_w = self.measure_lyric_text_width(display_text);
                    let natural_text_w = character_text_w
                        .map(|w| w / self.config.non_expanded_scale)
                        .unwrap_or(text_w);
                    let natural_w = 60.0 + natural_text_w;
                    let min_w = self.config.base_width + 35.0;
                    let max_w = self.config.lyrics_scroll_max_width.max(min_w);
                    if natural_w > max_w {
                        let fixed_w = max_w;
                        let available_text_w = (fixed_w - 59.0) * self.config.non_expanded_scale;
                        let loop_scroll = self.config.lyrics_scroll_infinite_loop
                            && lyric_character_data.is_none();
                        let full_text_w = if let Some(w) = character_text_w {
                            w
                        } else if loop_scroll {
                            self.measure_mini_lyric_render_width(display_text)
                        } else {
                            text_w * self.config.non_expanded_scale
                        };
                        let overflow = full_text_w - available_text_w;
                        let needs_scroll = overflow > 0.0;
                        self.lyric_scroll_active = needs_scroll;
                        if needs_scroll && self.lyric_transition >= 1.0 {
                            if let Some(data) = lyric_character_data {
                                self.lyric_scroll_offset = self.mini_lyric_character_scroll_offset(
                                    data,
                                    available_text_w,
                                    overflow,
                                );
                                if !is_paused {
                                    window.request_redraw();
                                }
                            } else if !is_paused && loop_scroll {
                                let cycle_w = full_text_w + self.mini_lyric_loop_gap();
                                if self.lyric_scroll_pause > 0.0 {
                                    self.lyric_scroll_pause -= dt / 60.0;
                                } else if cycle_w > 0.0 {
                                    self.lyric_scroll_offset += 0.8 * dt;
                                    self.lyric_scroll_offset %= cycle_w;
                                    if self.lyric_scroll_offset <= 0.0 {
                                        self.lyric_scroll_offset = 0.001;
                                    }
                                }
                                window.request_redraw();
                            } else if !is_paused && self.lyric_scroll_offset < overflow {
                                if self.lyric_scroll_pause > 0.0 {
                                    self.lyric_scroll_pause -= dt / 60.0;
                                } else {
                                    self.lyric_scroll_offset += 0.8 * dt;
                                    if self.lyric_scroll_offset >= overflow {
                                        self.lyric_scroll_offset = overflow;
                                    }
                                }
                                window.request_redraw();
                            } else if is_paused {
                                self.lyric_scroll_offset = 0.0;
                            }
                        } else {
                            self.lyric_scroll_offset = 0.0;
                        }
                        fixed_w
                    } else {
                        self.lyric_scroll_offset = 0.0;
                        natural_w.clamp(min_w, max_w)
                    }
                } else {
                    let display_text = if !self.current_lyric_text.is_empty() {
                        &self.current_lyric_text
                    } else {
                        &self.old_lyric_text
                    };
                    let text_w = self.measure_lyric_text_width(display_text);
                    self.lyric_scroll_offset = 0.0;
                    let min_w = self.config.base_width + 35.0;
                    let w: f32 = 60.0 + text_w;
                    w.clamp(min_w, min_w.max(450.0))
                }
            } else {
                self.config.base_width + 35.0
            }
        } else {
            self.lyric_scroll_offset = 0.0;
            self.config.base_width
        };
        if self.expanded {
            self.config.expanded_width * self.config.expanded_scale
        } else {
            target_base_w * self.config.non_expanded_scale
        }
    }
}

impl ApplicationHandler for App {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: ()) {
        // 捕获回调或权限回调唤醒主线程重绘
        crate::gpu::take_wake_pending();
        if let Some(win) = &self.window {
            win.request_redraw();
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Poll);
        if self.window.is_none() {
            Self::set_aumid();
            let (os_w, os_h) = Self::compute_os_size(&self.config);
            self.os_w = os_w;
            self.os_h = os_h;
            let attrs = Window::default_attributes()
                .with_title(WINDOW_TITLE)
                .with_inner_size(PhysicalSize::new(self.os_w, self.os_h))
                .with_transparent(true)
                .with_visible(false)
                .with_decorations(false)
                .with_resizable(false)
                .with_enabled_buttons(WindowButtons::empty())
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_skip_taskbar(true)
                .with_window_icon(get_app_icon());
            let window = Arc::new(event_loop.create_window(attrs).unwrap());

            if let Ok(handle) = window.window_handle()
                && let RawWindowHandle::Win32(win32_handle) = handle.as_raw()
            {
                self.hwnd_raw = Some(win32_handle.hwnd.get());
                let hwnd = HWND(win32_handle.hwnd.get() as _);
                crate::utils::win32::modify_window_ex_style(
                    hwnd,
                    WS_EX_TOOLWINDOW.0 as isize | WS_EX_NOACTIVATE.0 as isize,
                    0,
                );
                crate::utils::win32::modify_window_style(
                    hwnd,
                    0,
                    WS_MAXIMIZEBOX.0 as isize | WS_THICKFRAME.0 as isize,
                );
                if self.config.island_style == "liquid_glass" {
                    set_exclude_from_capture(hwnd, true);
                }
            }

            self.window = Some(window.clone());

            // 毛玻璃伴随窗口：只承载 DWM 宿主背板合成视觉，始终隐藏，由宿主背板按需显示
            let backdrop_attrs = Window::default_attributes()
                .with_title(BACKDROP_WINDOW_TITLE)
                .with_inner_size(PhysicalSize::new(1, 1))
                .with_transparent(true)
                .with_no_redirection_bitmap(true)
                .with_visible(false)
                .with_decorations(false)
                .with_resizable(false)
                .with_enabled_buttons(WindowButtons::empty())
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_skip_taskbar(true);
            if let Ok(backdrop) = event_loop.create_window(backdrop_attrs) {
                // 背板窗口只负责合成，不参与命中测试
                let _ = backdrop.set_cursor_hittest(false);
                if let Ok(handle) = backdrop.window_handle()
                    && let RawWindowHandle::Win32(win32_handle) = handle.as_raw()
                {
                    self.backdrop_hwnd_raw = Some(win32_handle.hwnd.get());
                }
                self.backdrop_window = Some(Arc::new(backdrop));
            } else {
                log::warn!("毛玻璃伴随窗口创建失败，毛玻璃将使用 CPU 截屏路径");
            }

            let mut monitor_opt = None;
            for _ in 0..10 {
                if let Some(monitor) = Self::get_target_monitor(&window, self.config.monitor_index)
                {
                    let size = monitor.size();
                    if size.width > 0 && size.height > 0 {
                        monitor_opt = Some(monitor);
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }

            if let Some(monitor) = monitor_opt {
                let mon_size = monitor.size();
                let mon_pos = monitor.position();
                self.last_mon_size = (mon_size.width, mon_size.height);
                self.last_mon_pos = (mon_pos.x, mon_pos.y);
                (self.win_x, self.win_y) = self.compute_window_position(mon_pos, mon_size);
                window.set_outer_position(PhysicalPosition::new(self.win_x, self.win_y));
                if self.config.island_style == "mica" {
                    clear_mica_cache();
                }
            }
            let context = Context::new(window.clone()).unwrap();
            let mut surface = Surface::new(&context, window.clone()).unwrap();
            surface
                .resize(
                    std::num::NonZeroU32::new(self.os_w.max(1)).unwrap(),
                    std::num::NonZeroU32::new(self.os_h.max(1)).unwrap(),
                )
                .unwrap();
            if let Ok(mut buf) = surface.buffer_mut() {
                for p in buf.iter_mut() {
                    *p = 0;
                }
                let _ = buf.present();
            }
            self.context = Some(context);
            self.surface = Some(surface);
            let is_light = window.theme() == Some(winit::window::Theme::Light);
            self.tray = Some(TrayManager::new(is_light));
            Self::enforce_topmost(&window, self.win_x, self.win_y, self.os_w, self.os_h);
            window.set_visible(true);
            window.request_redraw();
        }
    }
    fn window_event(&mut self, _event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if let Some(win) = &self.window
            && win.id() == id
        {
            match event {
                WindowEvent::ThemeChanged(theme) => {
                    let is_light = theme == winit::window::Theme::Light;
                    if let Some(tray) = self.tray.as_mut() {
                        tray.update_theme(is_light);
                    }
                    clear_liquid_glass_cache();
                }
                WindowEvent::Resized(_) if win.is_maximized() => {
                    win.set_maximized(false);
                }
                WindowEvent::CloseRequested => (),
                WindowEvent::HoveredFile(_) => (),
                WindowEvent::HoveredFileCancelled => (),
                WindowEvent::MouseWheel { delta, .. } => {
                    let (px, py) = get_global_cursor_pos();
                    self.handle_volume_wheel(delta, px, py);
                }
                WindowEvent::MouseInput {
                    state,
                    button: MouseButton::Left,
                    ..
                } => {
                    let (px, py) = get_global_cursor_pos();
                    self.handle_input(state, px, py);
                }
                WindowEvent::Touch(touch) => {
                    let (px, py) = (
                        (touch.location.x + self.win_x as f64) as i32,
                        (touch.location.y + self.win_y as f64) as i32,
                    );
                    self.touch_pos = touch.location;
                    match touch.phase {
                        TouchPhase::Started => {
                            self.touch_id = Some(touch.id);
                            self.handle_input(ElementState::Pressed, px, py);
                        }
                        TouchPhase::Moved => {
                            self.touch_id = Some(touch.id);
                        }
                        TouchPhase::Ended | TouchPhase::Cancelled => {
                            self.handle_input(ElementState::Released, px, py);
                            self.touch_id = None;
                        }
                    }
                }
                WindowEvent::RedrawRequested => {
                    if self.config.lyrics_filter_scope != LyricsFilterScope::Off {
                        self.refresh_lyrics_filter_regex();
                    }
                    let is_hidden = self.is_hidden();
                    let progress = self.expansion_progress();
                    self.ensure_gpu_island();
                    self.ensure_host_backdrop();
                    if self.surface.is_some() {
                        let dt =
                            (self.last_frame_time.elapsed().as_secs_f32() * 60.0).clamp(0.1, 3.0);
                        let sigmas = if self.config.motion_blur {
                            calculate_blur_sigmas(
                                self.spring_w.velocity,
                                self.spring_h.velocity,
                                self.spring_view.velocity,
                                self.spring_w.value,
                            )
                        } else {
                            (0.0, 0.0)
                        };
                        let mut media_info = self.media.get_info();
                        if self.seeking_progress && self.seeking_duration_ms > 0 {
                            media_info.position_ms = self.seeking_preview_ms;
                            media_info.last_update = Instant::now();
                        }
                        if !self.config.audio_gate {
                            self.audio.set_gate_override(false);
                        } else {
                            self.audio.set_gate_override(!is_hidden);
                        }
                        media_info.spectrum = self.audio.get_spectrum();
                        if !self.config.audio_gate {
                            media_info.spectrum = [0.0; 6];
                        }
                        let mut music_active = false;
                        if !media_info.title.is_empty() {
                            music_active = true;
                        }

                        let delay_ms = (self.config.lyrics_delay * 1000.0) as i64;
                        let lyric_current_pos = media_info.effective_position_ms(delay_ms);
                        let mut char_data =
                            if self.config.lyrics_char_highlight && self.config.show_lyrics {
                                media_info.current_character_data(delay_ms)
                            } else {
                                None
                            };
                        if char_data.is_some() && self.config.lyrics_filter_scope.filters_desktop()
                        {
                            let lyrics = media_info.lyrics.as_ref();
                            let is_filtered = lyrics
                                .and_then(|ly| {
                                    let idx = current_lyric_index(ly, lyric_current_pos)?;
                                    if self
                                        .lyrics_filter_regex_cache
                                        .as_ref()
                                        .is_some_and(|r| r.is_match(ly[idx].text.trim()))
                                    {
                                        Some(())
                                    } else {
                                        None
                                    }
                                })
                                .is_some();
                            if is_filtered {
                                char_data = None;
                            }
                        }
                        let current_char_progress = char_data.as_ref().and_then(|(chars, idx)| {
                            chars.get(*idx).map(|ch| {
                                if ch.e > ch.s {
                                    let raw = (lyric_current_pos.saturating_sub(ch.s)) as f32
                                        / (ch.e - ch.s) as f32;
                                    if *idx + 1 >= chars.len() {
                                        raw.clamp(0.0, 2.0)
                                    } else {
                                        raw.clamp(0.0, 1.0)
                                    }
                                } else {
                                    0.5
                                }
                            })
                        });
                        let lyric_line_has_characters = char_data.is_some();
                        let current_characters = char_data.as_ref().map(|(c, _)| *c);
                        let current_char_idx = char_data.map(|(_, i)| i);
                        let char_color_unplayed =
                            parse_hex_color(&self.config.lyrics_char_color_unplayed);
                        let char_color_played =
                            parse_hex_color(&self.config.lyrics_char_color_played);
                        let lyric_scroll_loop_gap =
                            MINI_LYRIC_LOOP_GAP * self.config.non_expanded_scale;

                        // 毛玻璃宿主背板：先同步背板形状与磨砂程度，再绘制只含暗色覆盖层的岛
                        let (backdrop_shape, backdrop_blur) = self.host_backdrop_frame(progress);
                        self.update_host_backdrop(backdrop_shape, backdrop_blur);

                        let params = crate::core::render::DrawIslandParams {
                            layout: crate::core::render::LayoutParams {
                                current_w: self.spring_w.value,
                                current_h: self.spring_h.value,
                                current_r: self.spring_r.value,
                                os_w: self.os_w,
                                os_h: self.os_h,
                                sigmas,
                                expansion_progress: progress,
                                view_offset: self.spring_view.value,
                                non_expanded_scale: self.config.non_expanded_scale,
                                expanded_scale: self.config.expanded_scale,
                                hide_progress: self.spring_hide.value,
                                dock_position: self.effective_dock_position,
                            },
                            media: crate::core::render::MediaParams {
                                media: &media_info,
                                music_active,
                            },
                            lyrics: crate::core::render::LyricsParams {
                                current_lyric: &self.current_lyric_text,
                                old_lyric: &self.old_lyric_text,
                                lyric_transition: self.lyric_transition,
                                lyric_scroll_active: self.lyric_scroll_active,
                                lyric_scroll_offset: self.lyric_scroll_offset,
                                lyric_scroll_loop: self.config.lyrics_scroll
                                    && self.config.lyrics_scroll_infinite_loop
                                    && !lyric_line_has_characters
                                    && self.lyric_scroll_active,
                                lyric_scroll_loop_gap,
                                current_characters,
                                current_char_idx,
                                current_char_progress,
                                char_color_unplayed,
                                char_color_played,
                                char_highlight: self.config.lyrics_char_highlight,
                                char_lift_animation: self.config.lyrics_char_lift_animation,
                            },
                            window: crate::core::render::WindowParams {
                                win_x: self.win_x,
                                win_y: self.win_y,
                                monitor_x: self.last_mon_pos.0,
                                monitor_y: self.last_mon_pos.1,
                                monitor_w: self.last_mon_size.0,
                                monitor_h: self.last_mon_size.1,
                            },
                            style: crate::core::render::StyleParams {
                                island_style: &self.config.island_style,
                                use_blur: self.config.motion_blur,
                                font_size: self.config.font_size,
                                mini_cover_shape: &self.config.mini_cover_shape,
                                expanded_cover_shape: &self.config.expanded_cover_shape,
                                cover_rotate: self.config.cover_rotate,
                                mini_controls: self.config.mini_controls,
                                lyrics_delay: self.config.lyrics_delay,
                                lyrics_filter_scope: self.config.lyrics_filter_scope,
                                lyrics_filter_regex: self.lyrics_filter_regex_cache.as_ref(),
                                dt,
                                liquid_glass_blur: self.config.liquid_glass_blur,
                                liquid_glass_refraction: self.config.liquid_glass_refraction,
                                liquid_glass_dispersion: self.config.liquid_glass_dispersion,
                                liquid_glass_tint: self.config.liquid_glass_tint,
                                liquid_glass_siri: self.config.liquid_glass_siri,
                                host_backdrop: self.host_backdrop.is_some(),
                            },
                        };
                        let mut gpu_error: Option<String> = None;
                        let widget_animating = if let Some(gpu) = self.gpu_island.as_mut() {
                            let liquid_scale = self.config.non_expanded_scale
                                + (self.config.expanded_scale - self.config.non_expanded_scale)
                                    * progress;
                            let frost_sigma = self.config.liquid_glass_blur * liquid_scale;
                            let animating =
                                music_active || sigmas.0.abs() > 0.01 || sigmas.1.abs() > 0.01;
                            let render_result = gpu.render(
                                (self.win_x, self.win_y),
                                (self.os_w, self.os_h),
                                frost_sigma,
                                animating,
                                |canvas, backdrop| draw_island_canvas(canvas, params, backdrop),
                            );
                            match render_result {
                                Ok(animating) => animating,
                                Err(error) => {
                                    gpu_error = Some(error);
                                    false
                                }
                            }
                        } else if let Some(surface) = self.surface.as_mut() {
                            draw_island(surface, params)
                        } else {
                            false
                        };
                        if let Some(error) = gpu_error {
                            self.fail_gpu_island(&error);
                            if let Some(win) = &self.window {
                                win.request_redraw();
                            }
                        }
                        if widget_animating && let Some(win) = &self.window {
                            win.request_redraw();
                        }
                    }
                }
                _ => (),
            }
        }
    }
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if crate::utils::updater::take_exit_requested() {
            Self::close_settings_window();
            event_loop.exit();
            return;
        }

        if self.media.is_disabled() {
            Self::close_settings_window();
            event_loop.exit();
            return;
        }

        let window = match self.window.clone() {
            Some(w) => w,
            None => return,
        };
        Self::enforce_topmost(&window, self.win_x, self.win_y, self.os_w, self.os_h);
        let frame_start = Instant::now();
        self.handle_tray_events(&window, event_loop);
        self.reload_config_if_changed(&window);

        let dt = (self.last_frame_time.elapsed().as_secs_f32() * 60.0).clamp(0.1, 3.0);
        self.last_frame_time = Instant::now();

        if !self.visible {
            std::thread::sleep(Duration::from_millis(16));
            return;
        }
        let (px, py) = if self.touch_id.is_some() {
            (
                (self.touch_pos.x + self.win_x as f64) as i32,
                (self.touch_pos.y + self.win_y as f64) as i32,
            )
        } else {
            get_global_cursor_pos()
        };
        let rel_x = px - self.win_x;
        let rel_y = py - self.win_y;
        let layout = self.compute_island_layout();
        let dock_bottom = layout.dock_bottom;
        let island_y = layout.island_y;
        let offset_x = layout.offset_x;
        let hide_distance = layout.hide_distance;
        let current_island_y = layout.current_island_y;
        let is_hovering_visible = is_point_in_rect(
            rel_x as f64,
            rel_y as f64,
            offset_x,
            current_island_y,
            self.spring_w.value as f64,
            self.spring_h.value as f64,
        );
        let hidden_handle_h = layout.hidden_handle_h;
        let hidden_handle_y = layout.hidden_handle_y;
        let is_on_hidden_handle = self.is_hidden()
            && is_point_in_rect(
                rel_x as f64,
                rel_y as f64,
                offset_x,
                hidden_handle_y,
                self.spring_w.value as f64,
                hidden_handle_h,
            );
        let hover_to_hide_available =
            self.config.hover_to_hide && !self.expanded && !self.manually_hidden;
        let (cursor_in_hover_inner, cursor_in_hover_outer) = if hover_to_hide_available {
            let d = self.config.hover_to_hide_distance as f64;
            let buffer = d * 0.5;
            (
                is_point_in_rect(
                    rel_x as f64,
                    rel_y as f64,
                    offset_x - d,
                    current_island_y - d,
                    self.spring_w.value as f64 + d * 2.0,
                    self.spring_h.value as f64 + d * 2.0,
                ),
                is_point_in_rect(
                    rel_x as f64,
                    rel_y as f64,
                    offset_x - d - buffer,
                    current_island_y - d - buffer,
                    self.spring_w.value as f64 + (d + buffer) * 2.0,
                    self.spring_h.value as f64 + (d + buffer) * 2.0,
                ),
            )
        } else {
            (false, false)
        };
        let block_auto_reveal = hover_to_hide_available && cursor_in_hover_inner;

        if self.frame_count.is_multiple_of(10) {
            self.is_fullscreen_suppressed = is_foreground_fullscreen();
            self.is_cursor_suppressed = is_cursor_hidden();
        }

        if self.frame_count.is_multiple_of(30) {
            self.update_dock_position_from_process(&window);
        }

        if self.is_fullscreen_suppressed || self.is_cursor_suppressed {
            let _ = window.set_cursor_hittest(false);
        } else {
            let clickable = !self.hover_hidden && (is_hovering_visible || is_on_hidden_handle);
            let _ = window.set_cursor_hittest(clickable);
        }

        let mut music_active = false;
        let media = self.media.get_info();
        if !media.title.is_empty() {
            self.last_media_playing = media.is_playing;
            music_active = true;
            let title_changed = media.title != self.last_media_title;
            let thumbnail_changed = media.thumbnail_hash != self.last_media_thumbnail_hash;
            if title_changed {
                self.last_media_title = media.title.clone();
                self.last_media_thumbnail_hash = media.thumbnail_hash;
                if thumbnail_changed {
                    if media.thumbnail_hash != 0 {
                        crate::ui::expanded::music_view::trigger_cover_flip();
                    } else {
                        crate::ui::expanded::music_view::clear_cover_cache();
                    }
                    crate::utils::backdrop::clear_dynamic_bg_cache();
                    window.request_redraw();
                }
            } else if thumbnail_changed {
                self.last_media_thumbnail_hash = media.thumbnail_hash;
                if media.thumbnail_hash != 0 {
                    crate::ui::expanded::music_view::trigger_cover_flip();
                } else {
                    crate::ui::expanded::music_view::clear_cover_cache();
                }
                crate::utils::backdrop::clear_dynamic_bg_cache();
                window.request_redraw();
            }
        }

        let is_paused_idle = music_active && !media.is_playing;
        let is_idle = !is_hovering_visible
            && !self.expanded
            && !self.is_dragging
            && (!music_active || is_paused_idle);
        if !self.config.auto_hide {
            self.auto_hidden = false;
            self.idle_timer = Instant::now();
        } else if media.is_playing
            && self.auto_hidden
            && !self.manually_hidden
            && !block_auto_reveal
        {
            self.auto_hidden = false;
            self.idle_timer = Instant::now();
            self.spring_hide.velocity = -0.65;
        } else if self.auto_hidden {
            if !block_auto_reveal && (is_on_hidden_handle || is_hovering_visible) {
                self.auto_hidden = false;
                self.idle_timer = Instant::now();
                self.spring_hide.velocity = -0.45;
            }
        } else if is_idle && !self.manually_hidden {
            if self.idle_timer.elapsed().as_secs_f32() > self.config.auto_hide_delay {
                self.auto_hidden = true;
            }
        } else if !self.manually_hidden && !is_idle {
            self.idle_timer = Instant::now();
        }

        if hover_to_hide_available && !self.auto_hidden {
            if !self.hover_hidden {
                if cursor_in_hover_inner {
                    self.hover_hidden = true;
                    self.hover_to_hide_enter_at = None;
                }
            } else if !cursor_in_hover_outer {
                match self.hover_to_hide_enter_at {
                    None => self.hover_to_hide_enter_at = Some(Instant::now()),
                    Some(t)
                        if t.elapsed()
                            >= Duration::from_secs_f64(self.config.hover_to_hide_delay as f64) =>
                    {
                        self.hover_hidden = false;
                        self.idle_timer = Instant::now();
                        self.spring_hide.velocity = -0.45;
                        self.hover_to_hide_enter_at = None;
                    }
                    _ => {}
                }
            } else {
                self.hover_to_hide_enter_at = None;
            }
        } else {
            self.hover_hidden = false;
            self.hover_to_hide_enter_at = None;
        }

        if self.seeking_progress && (is_left_button_pressed() || self.touch_id.is_some()) {
            let page_shift = self.spring_view.value * self.spring_w.value;
            let click_x = rel_x as f32 - page_shift;
            let bar_width = self.seeking_bar_right - self.seeking_bar_left;
            let ratio = if bar_width > 0.0 {
                ((click_x - self.seeking_bar_left) / bar_width).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let seek_ms = (ratio as f64 * self.seeking_duration_ms as f64) as u64;
            self.seeking_preview_ms = seek_ms;
            window.request_redraw();
        } else if self.seeking_progress {
            self.seeking_progress = false;
            if self.seeking_duration_ms > 0 {
                self.media.request_seek(self.seeking_preview_ms);
                window.request_redraw();
            }
        }

        let progress_hover_active = if self.seeking_progress {
            true
        } else if self.expanded && (self.spring_view.value as f64) < 0.5 {
            if let Some((bar_left, bar_right, bar_top, bar_hit_h)) = get_progress_bar_rect(
                offset_x as f32,
                island_y as f32,
                self.spring_w.value,
                &media,
                music_active,
                self.config.expanded_scale,
                &self.config.expanded_cover_shape,
            ) {
                let page_shift = self.spring_view.value * self.spring_w.value;
                let cx = rel_x as f32 - page_shift;
                let cy = rel_y as f32;
                let margin = 4.0 * self.config.expanded_scale;
                cx >= bar_left - margin
                    && cx <= bar_right + margin
                    && cy >= bar_top - margin
                    && cy <= bar_top + bar_hit_h + margin
            } else {
                false
            }
        } else {
            false
        };
        set_progress_hover(progress_hover_active);
        set_progress_dragging(self.seeking_progress);

        if self.is_dragging {
            let diff_y = if dock_bottom {
                py - self.drag_start_py
            } else {
                self.drag_start_py - py
            };
            if diff_y.abs() > 3 {
                self.drag_has_moved = true;
            }
            if hide_distance > 0.0 {
                let mut new_val = self.drag_start_hide_val + (diff_y as f32 / hide_distance as f32);
                new_val = new_val.clamp(0.0, 1.0);
                self.spring_hide.value = new_val;
                self.spring_hide.velocity = 0.0;
                window.request_redraw();
            }
        } else {
            let hide_target = if self.is_hidden() { 1.0 } else { 0.0 };
            let (stiffness, damping) = if self.is_hidden() {
                (0.12, 0.70)
            } else {
                (0.08, 0.78)
            };
            self.spring_hide
                .update_dt(hide_target, stiffness, damping, dt);
        }

        if self.spring_hide.velocity.abs() > 0.001
            || (self.spring_hide.value > 0.0 && self.spring_hide.value < 1.0)
        {
            window.request_redraw();
        }

        if self.expanded
            && !is_hovering_visible
            && (is_left_button_pressed() || self.touch_id.is_some())
        {
            self.expanded = false;
            self.widget_view = false;
            window.request_redraw();
        }

        if !self.expanded
            && is_hovering_visible
            && (is_left_button_pressed() || self.touch_id.is_some())
        {
            self.idle_timer = Instant::now();
        }

        self.frame_count += 1;

        let is_paused = music_active && !media.is_playing;
        let current_lyric_opt = if self.config.show_lyrics && !is_paused {
            if self.config.lyrics_filter_scope.filters_desktop() {
                self.current_filtered_lyric(&media)
            } else {
                media.current_lyric((self.config.lyrics_delay * 1000.0) as i64)
            }
        } else {
            None
        };
        if let Some(lyric) = current_lyric_opt {
            if lyric != self.current_lyric_text {
                self.old_lyric_text = self.current_lyric_text.clone();
                self.current_lyric_text = lyric.clone();
                self.lyric_transition = 0.0;
                self.lyric_scroll_offset = 0.0;
                self.lyric_scroll_pause = 0.0;
            }
        } else if !is_paused && music_active {
            let fallback = format!("{} - {}", media.title, media.artist);
            if self.current_lyric_text != fallback {
                if !self.current_lyric_text.is_empty() {
                    self.old_lyric_text = self.current_lyric_text.clone();
                }
                self.current_lyric_text = fallback;
                self.lyric_transition = 0.0;
                self.lyric_scroll_offset = 0.0;
                self.lyric_scroll_pause = 0.0;
            }
        } else if !is_paused && !self.current_lyric_text.is_empty() {
            self.old_lyric_text = self.current_lyric_text.clone();
            self.current_lyric_text = String::new();
            self.lyric_transition = 0.0;
            self.lyric_scroll_offset = 0.0;
            self.lyric_scroll_pause = 0.0;
        }

        if self.lyric_transition < 1.0 {
            self.lyric_transition += 0.05 * dt;
            if self.lyric_transition > 1.0 {
                self.lyric_transition = 1.0;
            }
            window.request_redraw();
        }

        let lyric_character_data = self.current_desktop_character_data(&media);
        let target_w = self.compute_lyric_target_width(
            &window,
            music_active,
            is_paused,
            lyric_character_data.as_ref(),
            dt,
        );
        let target_h = if self.expanded {
            self.config.expanded_height * self.config.expanded_scale
        } else {
            self.config.base_height * self.config.non_expanded_scale
        };
        let target_r = if self.expanded {
            32.0 * self.config.expanded_scale
        } else {
            (self.config.base_height * self.config.non_expanded_scale) / 2.0
        };
        let target_view = if self.widget_view { 1.0 } else { 0.0 };
        self.spring_w.update_dt(target_w, 0.10, 0.68, dt);
        self.spring_h.update_dt(target_h, 0.10, 0.68, dt);
        self.spring_r.update_dt(target_r, 0.10, 0.68, dt);
        self.spring_view.update_dt(target_view, 0.12, 0.68, dt);

        if self.expanded
            || (music_active && media.is_playing)
            || self.spring_w.velocity.abs() > 0.001
            || self.spring_h.velocity.abs() > 0.001
            || self.spring_r.velocity.abs() > 0.001
            || self.spring_view.velocity.abs() > 0.001
        {
            window.request_redraw();
        }
        let elapsed = frame_start.elapsed();
        let target_frame_time = Duration::from_micros(6944);
        if elapsed < target_frame_time {
            std::thread::sleep(target_frame_time - elapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_wheel_step_uses_standard_profile() {
        assert_eq!(volume_wheel_step(None), VOLUME_WHEEL_SLOW_STEP);
        assert_eq!(
            volume_wheel_step(Some(Duration::from_millis(181))),
            VOLUME_WHEEL_SLOW_STEP
        );
        assert_eq!(
            volume_wheel_step(Some(Duration::from_millis(180))),
            VOLUME_WHEEL_MEDIUM_STEP
        );
        assert_eq!(
            volume_wheel_step(Some(Duration::from_millis(80))),
            VOLUME_WHEEL_MEDIUM_STEP
        );
        assert_eq!(
            volume_wheel_step(Some(Duration::from_millis(79))),
            VOLUME_WHEEL_FAST_STEP
        );
    }

    #[test]
    fn wheel_delta_units_has_single_event_limit() {
        assert_eq!(
            wheel_delta_units(MouseScrollDelta::LineDelta(0.0, 3.0)),
            VOLUME_WHEEL_MAX_UNITS
        );
        assert_eq!(
            wheel_delta_units(MouseScrollDelta::LineDelta(0.0, -3.0)),
            -VOLUME_WHEEL_MAX_UNITS
        );
        assert_eq!(
            wheel_delta_units(MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                0.0, 60.0
            ))),
            0.5
        );
    }
}
