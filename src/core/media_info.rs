use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::lyrics::{LyricLine, current_lyric_index};
use crate::core::lyrics_ws::VOLUME_MAX_DEFAULT;

/// 播放位置外推的最大时长：超过此阈值说明时间基准已失效（如睡眠唤醒后
/// `Instant` 继续计时），不再外推，冻结在最后一次上报位置等待下次状态同步。
const MAX_EXTRAPOLATION: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub track_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub is_playing: bool,
    pub is_favorite: bool,
    pub thumbnail: Option<Arc<Vec<u8>>>,
    pub thumbnail_hash: u64,
    pub spectrum: [f32; 6],
    pub position_ms: u64,
    pub last_update: Instant,
    pub lyrics: Option<Arc<Vec<LyricLine>>>,
    pub duration_ms: u64,
    pub volume: Option<f32>,
    /// 音量上限：外部软件未上报时默认 100，协议绝对上限 300
    pub volume_max: f32,
    pub play_mode: String,
}

impl Default for MediaInfo {
    fn default() -> Self {
        Self {
            track_id: None,
            title: String::new(),
            artist: String::new(),
            is_playing: false,
            is_favorite: false,
            thumbnail: None,
            thumbnail_hash: 0,
            spectrum: [0.0; 6],
            position_ms: 0,
            last_update: Instant::now(),
            lyrics: None,
            duration_ms: 0,
            volume: None,
            volume_max: VOLUME_MAX_DEFAULT,
            play_mode: String::from("list"),
        }
    }
}

impl MediaInfo {
    pub fn effective_duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// 自上次上报以来可外推的时间量：超过阈值即视为时间基准失效并截断，
    /// 防止睡眠唤醒后进度条跳满、歌词跳到结尾。
    fn extrapolated_elapsed_ms(&self) -> u64 {
        let elapsed = self.last_update.elapsed();
        if elapsed > MAX_EXTRAPOLATION {
            0
        } else {
            elapsed.as_millis() as u64
        }
    }

    fn raw_position_ms(&self) -> u64 {
        if self.is_playing {
            self.position_ms
                .saturating_add(self.extrapolated_elapsed_ms())
        } else {
            self.position_ms
        }
    }

    pub fn current_lyric(&self, delay_ms: i64) -> Option<String> {
        let lyrics = self.lyrics.as_ref()?;
        if lyrics.is_empty() {
            return None;
        }

        let current_pos = (self.raw_position_ms() as i64 + delay_ms).max(0) as u64;

        current_lyric_index(lyrics, current_pos).map(|idx| lyrics[idx].text.clone())
    }

    pub fn effective_position_ms(&self, delay_ms: i64) -> u64 {
        (self.raw_position_ms() as i64 + delay_ms).max(0) as u64
    }
}
