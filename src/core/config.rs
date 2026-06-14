use serde::{Deserialize, Serialize};
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_AUTHOR: &str = "xiaotian2333";
pub const APP_HOMEPAGE: &str = "https://github.com/xiaotian2333/EchoMusic-Lyrics-WinIsland";
pub const WINDOW_TITLE: &str = "EchoMusic-Lyrics-WinIsland";
pub const TOP_OFFSET: i32 = 10;
pub const PADDING: f32 = 80.0;

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(from = "String", into = "String")]
#[derive(Default)]
pub enum DockPosition {
    #[default]
    TopCenter,
    TopLeft,
    TopRight,
    BottomCenter,
    BottomLeft,
    BottomRight,
}

impl DockPosition {
    pub fn is_bottom(&self) -> bool {
        matches!(
            self,
            Self::BottomCenter | Self::BottomLeft | Self::BottomRight
        )
    }

    pub fn is_left(&self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft)
    }

    pub fn is_right(&self) -> bool {
        matches!(self, Self::TopRight | Self::BottomRight)
    }

    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::TopCenter => "top_center",
            Self::TopLeft => "top_left",
            Self::TopRight => "top_right",
            Self::BottomCenter => "bottom_center",
            Self::BottomLeft => "bottom_left",
            Self::BottomRight => "bottom_right",
        }
    }
}

impl std::fmt::Display for DockPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for DockPosition {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "top_center" => Ok(Self::TopCenter),
            "top_left" => Ok(Self::TopLeft),
            "top_right" => Ok(Self::TopRight),
            "bottom_center" => Ok(Self::BottomCenter),
            "bottom_left" => Ok(Self::BottomLeft),
            "bottom_right" => Ok(Self::BottomRight),
            _ => Err(()),
        }
    }
}

impl From<String> for DockPosition {
    fn from(value: String) -> Self {
        value.parse().unwrap_or_default()
    }
}

impl From<DockPosition> for String {
    fn from(value: DockPosition) -> Self {
        value.as_str().to_string()
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(from = "String", into = "String")]
pub enum LyricsFilterScope {
    #[default]
    Off,
    Desktop,
    All,
}

impl LyricsFilterScope {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Desktop => "desktop",
            Self::All => "all",
        }
    }

    pub const fn filters_desktop(&self) -> bool {
        matches!(self, Self::Desktop | Self::All)
    }

    pub const fn filters_all(&self) -> bool {
        matches!(self, Self::All)
    }
}

impl std::str::FromStr for LyricsFilterScope {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "desktop" => Ok(Self::Desktop),
            "all" => Ok(Self::All),
            "off" => Ok(Self::Off),
            _ => Err(()),
        }
    }
}

impl From<String> for LyricsFilterScope {
    fn from(value: String) -> Self {
        value.parse().unwrap_or_default()
    }
}

impl From<LyricsFilterScope> for String {
    fn from(value: LyricsFilterScope) -> Self {
        value.as_str().to_string()
    }
}

pub const DEFAULT_LYRICS_FILTER_REGEX: &str = r"^([^：]*)：.*$|^([^:]*):.*$|^([^翻唱]*)翻唱.*$|^([^许可]*)许可.*$|^([^音乐人]*)音乐人.*$|^([^国风]*)国风.*$|^([^纯音乐]*)纯音乐.*$|^([^星曜计划]*)星曜计划.*$";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProcessDockRule {
    pub process_name: String,
    pub dock_position: DockPosition,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AppConfig {
    pub non_expanded_scale: f32,
    pub expanded_scale: f32,
    pub base_width: f32,
    pub base_height: f32,
    pub expanded_width: f32,
    pub expanded_height: f32,
    pub adaptive_border: bool,
    pub motion_blur: bool,
    #[serde(default = "default_island_style")]
    pub island_style: String,
    #[serde(default = "default_show_lyrics")]
    pub show_lyrics: bool,
    #[serde(default = "default_custom_font")]
    pub custom_font_path: Option<String>,
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
    #[serde(default = "default_auto_hide")]
    pub auto_hide: bool,
    #[serde(default = "default_auto_hide_delay")]
    pub auto_hide_delay: f32,
    #[serde(default = "default_hover_to_hide")]
    pub hover_to_hide: bool,
    #[serde(default = "default_hover_to_hide_distance")]
    pub hover_to_hide_distance: f32,
    #[serde(default = "default_hover_to_hide_delay")]
    pub hover_to_hide_delay: f32,
    #[serde(default = "default_check_for_updates")]
    pub check_for_updates: bool,
    #[serde(default = "default_update_check_interval")]
    pub update_check_interval: f32,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default = "default_lyrics_delay")]
    pub lyrics_delay: f64,
    #[serde(default = "default_lyrics_scroll")]
    pub lyrics_scroll: bool,
    #[serde(default = "default_lyrics_scroll_max_width")]
    pub lyrics_scroll_max_width: f32,
    #[serde(default = "default_lyrics_filter_scope")]
    pub lyrics_filter_scope: LyricsFilterScope,
    #[serde(default = "default_lyrics_filter_regex")]
    pub lyrics_filter_regex: String,
    #[serde(default = "default_position_x_offset")]
    pub position_x_offset: i32,
    #[serde(default = "default_position_y_offset")]
    pub position_y_offset: i32,
    #[serde(default = "default_dock_position")]
    pub dock_position: DockPosition,
    #[serde(default = "default_process_dock_rules")]
    pub process_dock_rules: Vec<ProcessDockRule>,
    #[serde(default = "default_monitor_index")]
    pub monitor_index: i32,
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    #[serde(default = "default_settings_theme")]
    pub settings_theme: String,
    #[serde(default = "default_mini_cover_shape")]
    pub mini_cover_shape: String,
    #[serde(default = "default_expanded_cover_shape")]
    pub expanded_cover_shape: String,
    #[serde(default = "default_cover_rotate")]
    pub cover_rotate: bool,
    #[serde(default = "default_audio_gate")]
    pub audio_gate: bool,
    #[serde(default = "default_auto_gate")]
    pub auto_gate: bool,
    #[serde(default = "default_mini_controls")]
    pub mini_controls: bool,
    #[serde(default = "default_lyrics_char_highlight")]
    pub lyrics_char_highlight: bool,
    #[serde(default = "default_lyrics_char_color_unplayed")]
    pub lyrics_char_color_unplayed: String,
    #[serde(default = "default_lyrics_char_color_played")]
    pub lyrics_char_color_played: String,
}

fn default_island_style() -> String {
    "default".to_string()
}

fn default_show_lyrics() -> bool {
    true
}

fn default_custom_font() -> Option<String> {
    None
}

fn default_auto_start() -> bool {
    false
}

fn default_auto_hide() -> bool {
    false
}

fn default_auto_hide_delay() -> f32 {
    5.0
}

fn default_hover_to_hide() -> bool {
    false
}

fn default_hover_to_hide_distance() -> f32 {
    80.0
}

fn default_hover_to_hide_delay() -> f32 {
    0.5
}

fn default_check_for_updates() -> bool {
    true
}

fn default_update_check_interval() -> f32 {
    4.0
}

fn default_language() -> String {
    "auto".to_string()
}

fn default_lyrics_delay() -> f64 {
    0.0
}

fn default_lyrics_scroll() -> bool {
    false
}

fn default_lyrics_scroll_max_width() -> f32 {
    300.0
}

fn default_lyrics_filter_scope() -> LyricsFilterScope {
    LyricsFilterScope::Desktop
}

fn default_lyrics_filter_regex() -> String {
    DEFAULT_LYRICS_FILTER_REGEX.to_string()
}

fn default_position_x_offset() -> i32 {
    0
}

fn default_position_y_offset() -> i32 {
    0
}

fn default_dock_position() -> DockPosition {
    DockPosition::TopCenter
}

fn default_process_dock_rules() -> Vec<ProcessDockRule> {
    vec![]
}

fn default_monitor_index() -> i32 {
    0
}

fn default_font_size() -> f32 {
    0.0
}

fn default_settings_theme() -> String {
    "system".to_string()
}

fn default_mini_cover_shape() -> String {
    "square".to_string()
}

fn default_expanded_cover_shape() -> String {
    "square".to_string()
}

fn default_cover_rotate() -> bool {
    false
}

fn default_audio_gate() -> bool {
    true
}

fn default_auto_gate() -> bool {
    true
}

fn default_mini_controls() -> bool {
    false
}

fn default_lyrics_char_highlight() -> bool {
    true
}

fn default_lyrics_char_color_unplayed() -> String {
    "auto".to_string()
}

fn default_lyrics_char_color_played() -> String {
    "auto".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            non_expanded_scale: 1.0,
            expanded_scale: 1.0,
            base_width: 120.0,
            base_height: 27.0,
            expanded_width: 360.0,
            expanded_height: 200.0,
            adaptive_border: false,
            motion_blur: true,
            island_style: "default".to_string(),
            show_lyrics: true,
            custom_font_path: None,
            auto_start: false,
            auto_hide: false,
            auto_hide_delay: 5.0,
            hover_to_hide: false,
            hover_to_hide_distance: 80.0,
            hover_to_hide_delay: 0.5,
            check_for_updates: true,
            update_check_interval: 4.0,
            language: "auto".to_string(),
            lyrics_delay: 0.0,
            lyrics_scroll: false,
            lyrics_scroll_max_width: 300.0,
            lyrics_filter_scope: LyricsFilterScope::Desktop,
            lyrics_filter_regex: DEFAULT_LYRICS_FILTER_REGEX.to_string(),
            position_x_offset: 0,
            position_y_offset: 0,
            dock_position: DockPosition::TopCenter,
            process_dock_rules: vec![],
            monitor_index: 0,
            font_size: 0.0,
            settings_theme: "system".to_string(),
            mini_cover_shape: "square".to_string(),
            expanded_cover_shape: "square".to_string(),
            cover_rotate: false,
            audio_gate: true,
            auto_gate: true,
            mini_controls: false,
            lyrics_char_highlight: true,
            lyrics_char_color_unplayed: "auto".to_string(),
            lyrics_char_color_played: "auto".to_string(),
        }
    }
}
