use crate::core::lyrics::{LyricCharacter, LyricLine, current_character_index, current_lyric_index};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub is_playing: bool,
    pub thumbnail: Option<Arc<Vec<u8>>>,
    pub thumbnail_hash: u64,
    pub spectrum: [f32; 6],
    pub position_ms: u64,
    pub last_update: Instant,
    pub lyrics: Option<Arc<Vec<LyricLine>>>,
    pub duration_ms: u64,
}

impl Default for MediaInfo {
    fn default() -> Self {
        Self {
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            is_playing: false,
            thumbnail: None,
            thumbnail_hash: 0,
            spectrum: [0.0; 6],
            position_ms: 0,
            last_update: Instant::now(),
            lyrics: None,
            duration_ms: 0,
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

    pub fn current_character_data(
        &self,
        delay_ms: i64,
    ) -> Option<(&[LyricCharacter], usize)> {
        let lyrics = self.lyrics.as_ref()?;
        let current_pos = self.effective_position_ms(delay_ms);
        let line_idx = current_lyric_index(lyrics, current_pos)?;
        let line = &lyrics[line_idx];
        let characters = line.characters.as_ref()?;
        let char_idx = current_character_index(characters, current_pos)?;
        Some((characters.as_slice(), char_idx))
    }
}
