use std::fs;
use std::path::PathBuf;

use crate::core::config::{AppConfig, is_valid_color};

pub fn get_config_path() -> PathBuf {
    let mut path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push(".echomusic-lyrics-winisland");
    if !path.exists() {
        let _ = fs::create_dir_all(&path);
    }
    path.push("config.toml");
    path
}
pub fn load_config() -> AppConfig {
    let path = get_config_path();
    let mut config: AppConfig = if let Ok(content) = fs::read_to_string(path)
        && let Ok(config) = toml::from_str(&content)
    {
        config
    } else {
        let default = AppConfig::default();
        save_config(&default);
        return default;
    };
    config.non_expanded_scale = config.non_expanded_scale.clamp(0.5, 5.0);
    config.expanded_scale = config.expanded_scale.clamp(0.5, 5.0);
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
    config.lyrics_scroll_max_width = config.lyrics_scroll_max_width.max(config.base_width + 35.0);
    if !config.spectrum_wheel_volume {
        config.non_expanded_wheel_volume = false;
    }
    if !is_valid_color(&config.lyrics_char_color_unplayed) {
        config.lyrics_char_color_unplayed = AppConfig::default().lyrics_char_color_unplayed;
    }
    if !is_valid_color(&config.lyrics_char_color_played) {
        config.lyrics_char_color_played = AppConfig::default().lyrics_char_color_played;
    }
    config
}
pub fn save_config(config: &AppConfig) {
    let path = get_config_path();
    if let Ok(content) = toml::to_string_pretty(config) {
        let _ = fs::write(path, content);
    }
}
