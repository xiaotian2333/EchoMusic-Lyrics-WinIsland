use std::sync::Arc;
use std::time::Instant;

use crate::core::lyrics::{LyricLine, current_lyric_index};

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub track_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
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
    pub play_mode: String,
}

impl Default for MediaInfo {
    fn default() -> Self {
        Self {
            track_id: None,
            title: String::new(),
            artist: String::new(),
            album: String::new(),
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
            play_mode: String::from("list"),
        }
    }
}

impl MediaInfo {
    pub fn effective_duration_ms(&self) -> u64 {
        self.duration_ms
    }

    pub fn current_lyric(&self, delay_ms: i64) -> Option<String> {
        let lyrics = self.lyrics.as_ref()?;
        if lyrics.is_empty() {
            return None;
        }

        let raw_pos = if self.is_playing {
            self.position_ms
                .saturating_add(self.last_update.elapsed().as_millis() as u64)
        } else {
            self.position_ms
        };
        let current_pos = (raw_pos as i64 + delay_ms).max(0) as u64;

        current_lyric_index(lyrics, current_pos).map(|idx| lyrics[idx].text.clone())
    }

    pub fn effective_position_ms(&self, delay_ms: i64) -> u64 {
        let raw_pos = if self.is_playing {
            self.position_ms
                .saturating_add(self.last_update.elapsed().as_millis() as u64)
        } else {
            self.position_ms
        };
        (raw_pos as i64 + delay_ms).max(0) as u64
    }
}
