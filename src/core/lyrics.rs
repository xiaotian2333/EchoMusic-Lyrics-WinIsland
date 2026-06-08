use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Default, Debug)]
pub struct LyricLine {
    pub time_ms: u64,
    pub text: String,
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
