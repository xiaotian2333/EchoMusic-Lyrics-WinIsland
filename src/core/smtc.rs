#[cfg(windows)]
#[path = "smtc_windows.rs"]
mod imp;

#[cfg(not(windows))]
mod imp {
    use crate::core::lyrics::{LyricLine, MusicData, current_lyric_index};
    use crate::core::lyrics_ws::{LyricsWsEvent, LyricsWsHandle, start_lyrics_ws_server};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::sync::{mpsc, watch};
    use tokio_util::sync::CancellationToken;

    pub const TARGET_MEDIA_APP_ID: &str = "websocket";

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ThumbnailSource {
        None,
        Smtc,
        MusicData,
    }

    #[derive(Clone, Debug)]
    pub struct MediaInfo {
        pub title: String,
        pub artist: String,
        pub album: String,
        pub is_playing: bool,
        pub thumbnail: Option<Arc<Vec<u8>>>,
        pub thumbnail_hash: u64,
        pub thumbnail_source: ThumbnailSource,
        pub spectrum: [f32; 6],
        pub position_ms: u64,
        pub last_update: Instant,
        pub last_thumbnail_fetch: Instant,
        pub lyrics: Option<Arc<Vec<LyricLine>>>,
        pub last_smtc_pos: u64,
        pub duration_secs: u64,
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
                thumbnail_source: ThumbnailSource::None,
                spectrum: [0.0; 6],
                position_ms: 0,
                last_update: Instant::now(),
                last_thumbnail_fetch: Instant::now() - Duration::from_secs(10),
                lyrics: None,
                last_smtc_pos: 0,
                duration_secs: 0,
                duration_ms: 0,
            }
        }
    }

    impl MediaInfo {
        pub fn effective_duration_ms(&self) -> u64 {
            if self.duration_ms > 0 {
                self.duration_ms
            } else if self.duration_secs > 0 {
                self.duration_secs * 1000
            } else {
                0
            }
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
    }

    pub struct SmtcListener {
        info_rx: watch::Receiver<MediaInfo>,
        lyrics_ws_handle: LyricsWsHandle,
        cancel_token: CancellationToken,
    }

    impl SmtcListener {
        pub fn new() -> Self {
            let (info_tx, info_rx) = watch::channel(MediaInfo::default());
            let (lyrics_event_tx, lyrics_event_rx) = mpsc::unbounded_channel();
            let cancel_token = CancellationToken::new();
            let lyrics_ws_handle = start_lyrics_ws_server(lyrics_event_tx, cancel_token.clone());
            spawn_bridge_event_loop(
                info_tx,
                lyrics_event_rx,
                lyrics_ws_handle.clone(),
                cancel_token.clone(),
            );

            Self {
                info_rx,
                lyrics_ws_handle,
                cancel_token,
            }
        }

        pub fn get_info(&self) -> MediaInfo {
            self.info_rx.borrow().clone()
        }

        pub fn request_seek(&self, position_ms: u64) {
            self.lyrics_ws_handle.seek(position_ms);
        }

        pub fn request_toggle_play(&self) {
            // TODO: 未来 WebSocket 协议稳定后，在这里发送 toggle_play 命令。
        }

        pub fn request_next(&self) {
            // TODO: 未来 WebSocket 协议稳定后，在这里发送 next 命令。
        }

        pub fn request_prev(&self) {
            // TODO: 未来 WebSocket 协议稳定后，在这里发送 prev 命令。
        }
    }

    impl Drop for SmtcListener {
        fn drop(&mut self) {
            self.cancel_token.cancel();
        }
    }

    fn spawn_bridge_event_loop(
        info_tx: watch::Sender<MediaInfo>,
        mut lyrics_event_rx: mpsc::UnboundedReceiver<LyricsWsEvent>,
        lyrics_ws_handle: LyricsWsHandle,
        cancel: CancellationToken,
    ) {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    event = lyrics_event_rx.recv() => {
                        let Some(event) = event else {
                            break;
                        };
                        match event {
                            LyricsWsEvent::Connected | LyricsWsEvent::Subscribe => {
                                lyrics_ws_handle.request_track_lyrics();
                            }
                            LyricsWsEvent::MusicData(music_data) => {
                                apply_music_data(&info_tx, music_data);
                            }
                        }
                    }
                }
            }
        });
    }

    fn apply_music_data(info_tx: &watch::Sender<MediaInfo>, music_data: MusicData) {
        let metadata = music_data.metadata;
        let mut info = info_tx.borrow().clone();
        let song_changed = info.title != metadata.title || info.artist != metadata.artist;

        info.title = metadata.title;
        info.artist = metadata.artist;
        info.album = String::new();
        // TODO: 未来播放状态、进度和时长都由 WebSocket 更新；当前先视为播放中，避免歌词入口被暂停态隐藏。
        info.is_playing = true;
        info.position_ms = 0;
        info.last_smtc_pos = 0;
        info.duration_secs = 0;
        info.duration_ms = 0;
        info.last_update = Instant::now();
        // TODO: 未来频谱数据由 WebSocket 高频推送；当前跨平台 stub 先保持空频谱。
        info.spectrum = [0.0; 6];
        info.lyrics = Some(music_data.lyrics);

        if song_changed {
            info.thumbnail = None;
            info.thumbnail_hash = 0;
            info.thumbnail_source = ThumbnailSource::None;
        }

        if let Some(cover) = metadata.cover
            && is_supported_image_bytes(&cover)
        {
            let hash = hash_thumbnail_bytes(&cover);
            info.thumbnail = Some(cover);
            info.thumbnail_hash = hash;
            info.thumbnail_source = ThumbnailSource::MusicData;
            info.last_thumbnail_fetch = Instant::now();
        }

        let _ = info_tx.send(info);
    }

    fn hash_thumbnail_bytes(bytes: &[u8]) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        bytes.hash(&mut hasher);
        hasher.finish()
    }

    fn is_supported_image_bytes(bytes: &[u8]) -> bool {
        bytes.starts_with(&[0xFF, 0xD8, 0xFF])
            || bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
            || bytes.starts_with(b"GIF87a")
            || bytes.starts_with(b"GIF89a")
            || (bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP")
    }
}

pub use self::imp::*;
