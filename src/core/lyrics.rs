use serde_json::Value;
use std::sync::Arc;

const MAX_COVER_IMAGE_BYTES: usize = 5 * 1024 * 1024;

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
pub struct MusicMetadata {
    pub track_id: Option<String>,
    pub title: String,
    pub artist: String,
    pub artists: Vec<String>,
    pub cover: Option<Arc<Vec<u8>>>,
}

#[derive(Clone, Default, Debug)]
pub struct MusicData {
    pub metadata: MusicMetadata,
    pub lyrics: Arc<Vec<LyricLine>>,
}

pub fn parse_music_data_payload(payload: &Value) -> Option<MusicData> {
    let metadata = payload.get("Metadata")?;
    let title = metadata.get("title")?.as_str()?.trim().to_string();
    if title.is_empty() {
        return None;
    }

    let track_id = metadata
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let artist = metadata
        .get("artist")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let artists = metadata
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
    let cover = metadata
        .get("cover_base64")
        .and_then(|v| v.as_str())
        .and_then(|cover_base64| match decode_cover_base64(cover_base64) {
            Ok(cover) => cover.map(Arc::new),
            Err(err) => {
                log::warn!("已忽略无效 MusicData 封面: {}", err);
                None
            }
        });

    let lyrics = payload
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

    let mut lyrics: Vec<LyricLine> = lyrics;
    lyrics.sort_by_key(|line| line.time_ms);

    Some(MusicData {
        metadata: MusicMetadata {
            track_id,
            title,
            artist,
            artists,
            cover,
        },
        lyrics: Arc::new(lyrics),
    })
}

fn decode_cover_base64(input: &str) -> Result<Option<Vec<u8>>, &'static str> {
    if input.trim().is_empty() {
        return Ok(None);
    }
    if input.trim_start().starts_with("data:") || input.contains(',') {
        return Err("封面必须是纯 Base64，不应包含 data URL 前缀");
    }

    let max_base64_chars = MAX_COVER_IMAGE_BYTES.div_ceil(3) * 4 + 4;
    let mut cleaned = Vec::new();
    for byte in input.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        cleaned.push(byte);
        if cleaned.len() > max_base64_chars {
            return Err("封面 Base64 超过大小限制");
        }
    }

    if cleaned.is_empty() {
        return Ok(None);
    }

    let first_padding = cleaned.iter().position(|&byte| byte == b'=');
    let explicit_padding = if let Some(idx) = first_padding {
        if cleaned[idx..].iter().any(|&byte| byte != b'=') {
            return Err("封面 Base64 padding 位置非法");
        }
        let padding = cleaned.len() - idx;
        if padding > 2 || cleaned.len() % 4 != 0 {
            return Err("封面 Base64 padding 非法");
        }
        padding
    } else {
        0
    };

    for &byte in cleaned.iter().take(cleaned.len() - explicit_padding) {
        if base64_value(byte).is_none() {
            return Err("封面 Base64 包含非法字符");
        }
    }

    let remainder = cleaned.len() % 4;
    if explicit_padding == 0 && remainder == 1 {
        return Err("封面 Base64 长度非法");
    }
    let implicit_padding = if explicit_padding == 0 {
        match remainder {
            0 => 0,
            2 => 2,
            3 => 1,
            _ => return Err("封面 Base64 长度非法"),
        }
    } else {
        0
    };
    let total_padding = explicit_padding + implicit_padding;
    let padded_len = cleaned.len() + implicit_padding;
    let output_len = padded_len / 4 * 3 - total_padding;
    if output_len > MAX_COVER_IMAGE_BYTES {
        return Err("封面图片超过大小限制");
    }

    let mut output = Vec::with_capacity(output_len);
    for chunk_start in (0..padded_len).step_by(4) {
        let mut values = [0u8; 4];
        let mut is_padding = [false; 4];
        for i in 0..4 {
            let idx = chunk_start + i;
            let byte = cleaned.get(idx).copied().unwrap_or(b'=');
            if byte == b'=' {
                is_padding[i] = true;
            } else if let Some(value) = base64_value(byte) {
                values[i] = value;
            } else {
                return Err("封面 Base64 包含非法字符");
            }
        }
        if is_padding[0] || is_padding[1] || (is_padding[2] && !is_padding[3]) {
            return Err("封面 Base64 padding 非法");
        }

        output.push((values[0] << 2) | (values[1] >> 4));
        if !is_padding[2] {
            output.push((values[1] << 4) | (values[2] >> 2));
        }
        if !is_padding[3] {
            output.push((values[2] << 6) | values[3]);
        }
    }

    Ok(Some(output))
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_music_data_payload_accepts_new_shape() {
        let payload = json!({
            "Metadata": {
                "id": "123456",
                "title": " 歌曲名 ",
                "artist": "歌手 A、歌手 B",
                "artists": ["歌手 A", "", " 歌手 B "],
                "cover_base64": "AQIDBA=="
            },
            "lyrics": [
                { "time_ms": 15620, "text": "第二句歌词" },
                { "time_ms": 3430, "text": "第一句歌词" }
            ]
        });

        let music_data = parse_music_data_payload(&payload).expect("应能解析 MusicData");
        assert_eq!(music_data.metadata.track_id.as_deref(), Some("123456"));
        assert_eq!(music_data.metadata.title, "歌曲名");
        assert_eq!(music_data.metadata.artist, "歌手 A、歌手 B");
        assert_eq!(music_data.metadata.artists, vec!["歌手 A", "歌手 B"]);
        assert_eq!(
            music_data.metadata.cover.as_deref().map(Vec::as_slice),
            Some(&[1, 2, 3, 4][..])
        );
        assert_eq!(music_data.lyrics[0].time_ms, 3430);
        assert_eq!(music_data.lyrics[1].time_ms, 15620);
    }

    #[test]
    fn parse_music_data_payload_accepts_empty_lyrics() {
        let payload = json!({
            "Metadata": { "title": "歌曲名", "cover_base64": "" },
            "lyrics": []
        });

        let music_data = parse_music_data_payload(&payload).expect("空歌词 MusicData 应被接受");
        assert_eq!(music_data.metadata.title, "歌曲名");
        assert!(music_data.lyrics.is_empty());
    }

    #[test]
    fn parse_music_data_payload_rejects_track_field() {
        let payload = json!({
            "track": { "title": "旧字段" },
            "lyrics": [{ "time_ms": 0, "text": "歌词" }]
        });

        assert!(parse_music_data_payload(&payload).is_none());
    }

    #[test]
    fn parse_music_data_payload_ignores_invalid_cover_but_keeps_lyrics() {
        let payload = json!({
            "Metadata": {
                "title": "歌曲名",
                "cover_base64": "非法 base64"
            },
            "lyrics": [{ "time_ms": 0, "text": "歌词" }]
        });

        let music_data = parse_music_data_payload(&payload).expect("非法封面不应影响歌词");
        assert!(music_data.metadata.cover.is_none());
        assert_eq!(music_data.lyrics.len(), 1);
    }

    #[test]
    fn decode_cover_base64_rejects_data_url_prefix() {
        assert!(decode_cover_base64("data:image/png;base64,AQID").is_err());
    }

    #[test]
    fn decode_cover_base64_rejects_oversized_input() {
        let input = "A".repeat(MAX_COVER_IMAGE_BYTES.div_ceil(3) * 4 + 8);
        assert!(decode_cover_base64(&input).is_err());
    }
}
