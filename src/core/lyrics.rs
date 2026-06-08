use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Default, Debug)]
pub struct LyricLine {
    pub time_ms: u64,
    pub text: String,
}

pub fn current_lyric_index(lyrics: &[LyricLine], current_pos: u64) -> Option<usize> {
    if lyrics.is_empty() {
        return None;
    }
    match lyrics.binary_search_by_key(&current_pos, |line| line.time_ms) {
        Ok(idx) => Some(idx),
        Err(idx) => idx.checked_sub(1),
    }
}

pub fn filtered_lyric_text<F>(
    lyrics: &[LyricLine],
    current_idx: usize,
    title: &str,
    matcher: F,
) -> String
where
    F: Fn(&str) -> bool,
{
    if current_idx >= lyrics.len() {
        return title.to_string();
    }
    let current = lyrics[current_idx].text.trim();
    if !matcher(current) {
        return current.to_string();
    }
    lyrics[..current_idx]
        .iter()
        .rev()
        .map(|line| line.text.trim())
        .find(|text| !text.is_empty() && !matcher(text))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| title.to_string())
}

#[derive(Clone, Default, Debug)]
pub struct TrackLyrics {
    pub track_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub artists: Vec<String>,
    pub lyrics: Arc<Vec<LyricLine>>,
}

pub fn parse_track_lyrics_payload(payload: &Value) -> Option<TrackLyrics> {
    let track = payload.get("track")?;
    let title = track.get("title")?.as_str()?.trim().to_string();
    if title.is_empty() {
        return None;
    }

    let track_id = track
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let artist = track
        .get("artist")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let artists = track
        .get("artists")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut lyrics = payload
        .get("lyrics")?
        .as_array()?
        .iter()
        .filter_map(|line| {
            let text = line.get("text")?.as_str()?.trim().to_string();
            let time_ms = line.get("time_ms")?.as_f64()?;
            if !time_ms.is_finite() || time_ms < 0.0 {
                return None;
            }
            Some(LyricLine {
                time_ms: time_ms.round() as u64,
                text,
            })
        })
        .collect::<Vec<_>>();

    if lyrics.is_empty() {
        return None;
    }

    lyrics.sort_by_key(|line| line.time_ms);

    Some(TrackLyrics {
        track_id,
        title,
        artist,
        artists,
        lyrics: Arc::new(lyrics),
    })
}
