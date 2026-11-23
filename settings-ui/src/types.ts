/* 与 Rust 侧契约一一对应：
   AppConfig/ProcessDockRule 见 src/core/config.rs，SettingsState/AppInfo 见 src/window/settings/mod.rs。
   serde 未启用 rename_all，序列化保留 snake_case。 */

export interface ProcessDockRule {
  process_name: string
  dock_position: string
}

export interface AppConfig {
  non_expanded_scale: number
  expanded_scale: number
  base_width: number
  base_height: number
  expanded_width: number
  expanded_height: number
  motion_blur: boolean
  island_style: string
  liquid_glass_blur: number
  liquid_glass_refraction: number
  liquid_glass_dispersion: number
  liquid_glass_tint: number
  liquid_glass_siri: boolean
  liquid_glass_compat_capture: boolean
  glass_blur: number
  lyrics_transition_animation: string
  show_lyrics: boolean
  custom_font_path: string | null
  auto_start: boolean
  auto_hide: boolean
  auto_hide_delay: number
  hover_to_hide: boolean
  hover_to_hide_distance: number
  hover_to_hide_delay: number
  spectrum_wheel_volume: boolean
  non_expanded_wheel_volume: boolean
  volume_wheel_hint: boolean
  check_for_updates: boolean
  update_check_interval: number
  language: string
  lyrics_delay: number
  lyrics_scroll: boolean
  lyrics_scroll_max_width: number
  lyrics_scroll_infinite_loop: boolean
  lyrics_filter_scope: string
  lyrics_filter_regex: string
  position_x_offset: number
  position_y_offset: number
  dock_position: string
  process_dock_rules: ProcessDockRule[]
  monitor_index: number
  font_size: number
  settings_theme: string
  mini_cover_shape: string
  expanded_cover_shape: string
  cover_rotate: boolean
  audio_gate: boolean
  mini_controls: boolean
  lyrics_char_highlight: boolean
  lyrics_char_lift_animation: boolean
  lyrics_char_color_unplayed: string
  lyrics_char_color_played: string
}

export interface AppInfo {
  version: string
  author: string
  homepage: string
  icon_png: number[]
  icon_dark_png: number[]
}

export interface SettingsState {
  config: AppConfig
  current_lang: string
  translations: Record<string, string>
  monitors: string[]
  app: AppInfo
  lyrics_filter_regex_valid: boolean
}

/* withGlobalTauri 下注入的 Tauri v2 全局 API（只声明本页用到的部分） */
interface TauriCore {
  invoke?: (command: string, args?: Record<string, unknown>) => Promise<unknown>
}

interface TauriEvent {
  listen?: (event: string, handler: (event: { payload: unknown }) => void) => Promise<() => void>
}

interface TauriGlobalApi {
  core?: TauriCore
  event?: TauriEvent
}

declare global {
  interface Window {
    __TAURI__?: TauriGlobalApi
  }
}
