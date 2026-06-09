use crate::core::lyrics::MusicData;
use crate::core::lyrics_ws::{LyricsWsEvent, LyricsWsHandle, PlayAction, start_lyrics_ws_server};
use crate::core::media_info::MediaInfo;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
enum PlaybackCommand {
    Toggle,
    Next,
    Prev,
}

pub struct WsMediaListener {
    info_rx: watch::Receiver<MediaInfo>,
    seek_tx: mpsc::UnboundedSender<u64>,
    playback_tx: mpsc::UnboundedSender<PlaybackCommand>,
    _cancel_token: CancellationToken,
}

impl WsMediaListener {
    pub fn new() -> Self {
        let (info_tx, info_rx) = watch::channel(MediaInfo::default());
        let (seek_tx, seek_rx) = mpsc::unbounded_channel();
        let (playback_tx, playback_rx) = mpsc::unbounded_channel();
        let (lyrics_event_tx, lyrics_event_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();
        let lyrics_ws_handle = start_lyrics_ws_server(lyrics_event_tx, cancel_token.clone());

        let cancel = cancel_token.clone();
        let handle = lyrics_ws_handle.clone();
        tokio::spawn(async move {
            ws_media_loop(
                info_tx,
                seek_rx,
                playback_rx,
                lyrics_event_rx,
                handle,
                cancel,
            )
            .await;
        });

        Self {
            info_rx,
            seek_tx,
            playback_tx,
            _cancel_token: cancel_token,
        }
    }

    pub fn get_info(&self) -> MediaInfo {
        self.info_rx.borrow().clone()
    }

    pub fn request_seek(&self, position_ms: u64) {
        let _ = self.seek_tx.send(position_ms);
    }

    pub fn request_toggle_play(&self) {
        let _ = self.playback_tx.send(PlaybackCommand::Toggle);
    }

    pub fn request_next(&self) {
        let _ = self.playback_tx.send(PlaybackCommand::Next);
    }

    pub fn request_prev(&self) {
        let _ = self.playback_tx.send(PlaybackCommand::Prev);
    }
}

impl Drop for WsMediaListener {
    fn drop(&mut self) {
        self._cancel_token.cancel();
    }
}

async fn ws_media_loop(
    info_tx: watch::Sender<MediaInfo>,
    mut seek_rx: mpsc::UnboundedReceiver<u64>,
    mut playback_rx: mpsc::UnboundedReceiver<PlaybackCommand>,
    mut lyrics_event_rx: mpsc::UnboundedReceiver<LyricsWsEvent>,
    lyrics_ws_handle: LyricsWsHandle,
    cancel: CancellationToken,
) {
    let mut state = MediaInfo::default();
    let _ = info_tx.send(state.clone());

    let mut state_request_timer = tokio::time::interval(Duration::from_secs(2));
    state_request_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,

            _ = state_request_timer.tick() => {
                if !state.title.is_empty() && state.is_playing {
                    lyrics_ws_handle.get_playback_state();
                }
            }

            seek = seek_rx.recv() => {
                let Some(pos) = seek else { break };
                lyrics_ws_handle.seek(pos);
                state.position_ms = pos;
                state.last_update = Instant::now();
                let _ = info_tx.send(state.clone());
            }

            cmd = playback_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    PlaybackCommand::Toggle => lyrics_ws_handle.toggle_play(),
                    PlaybackCommand::Next => lyrics_ws_handle.next(),
                    PlaybackCommand::Prev => lyrics_ws_handle.prev(),
                }
            }

            event = lyrics_event_rx.recv() => {
                let Some(event) = event else { break };
                handle_ws_event(event, &mut state, &info_tx, &lyrics_ws_handle);
            }
        }
    }
}

fn handle_ws_event(
    event: LyricsWsEvent,
    state: &mut MediaInfo,
    info_tx: &watch::Sender<MediaInfo>,
    lyrics_ws_handle: &LyricsWsHandle,
) {
    match event {
        LyricsWsEvent::Connected | LyricsWsEvent::Subscribe => {
            lyrics_ws_handle.request_track_lyrics();
        }
        LyricsWsEvent::MusicData(music_data) => {
            let was_empty = state.title.trim().is_empty();
            apply_music_data(state, info_tx, music_data);
            if was_empty {
                lyrics_ws_handle.get_playback_state();
            }
        }
        LyricsWsEvent::PlaybackState {
            position_ms,
            duration_ms,
            is_playing,
        } => {
            if position_ms > 0 || duration_ms > 0 {
                state.position_ms = position_ms;
                state.last_update = Instant::now();
            }
            if duration_ms > 0 {
                state.duration_ms = duration_ms;
            }
            state.is_playing = is_playing;
            let _ = info_tx.send(state.clone());
        }
        LyricsWsEvent::PlaybackAction {
            action,
            position_ms,
        } => {
            match action {
                PlayAction::Play => {
                    state.is_playing = true;
                    state.position_ms = position_ms;
                    state.last_update = Instant::now();
                }
                PlayAction::Pause => {
                    state.is_playing = false;
                    state.position_ms = position_ms;
                    state.last_update = Instant::now();
                }
                PlayAction::Seek => {
                    state.position_ms = position_ms;
                    state.last_update = Instant::now();
                }
                PlayAction::Next | PlayAction::Prev => {
                    state.title.clear();
                    state.artist.clear();
                    state.lyrics = None;
                    state.thumbnail = None;
                    state.thumbnail_hash = 0;
                    state.position_ms = 0;
                    state.last_update = Instant::now();
                }
            }
            let _ = info_tx.send(state.clone());
        }
    }
}

fn normalize_match_text(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn text_matches(left: &str, right: &str) -> bool {
    let left = normalize_match_text(left);
    let right = normalize_match_text(right);
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left == right || left.contains(&right) || right.contains(&left)
}

fn artist_matches(metadata_artist: &str, metadata_artists: &[String], media_artist: &str) -> bool {
    if media_artist.trim().is_empty()
        || (metadata_artist.trim().is_empty() && metadata_artists.is_empty())
    {
        return true;
    }
    if text_matches(metadata_artist, media_artist) {
        return true;
    }
    metadata_artists
        .iter()
        .any(|artist| text_matches(artist, media_artist))
}

fn hash_thumbnail_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn apply_music_data(
    state: &mut MediaInfo,
    info_tx: &watch::Sender<MediaInfo>,
    music_data: MusicData,
) {
    let metadata = &music_data.metadata;

    if state.title.trim().is_empty() {
        state.title = metadata.title.clone();
        state.artist = metadata.artist.clone();
        state.lyrics = Some(music_data.lyrics);
        if let Some(ref cover) = metadata.cover {
            state.thumbnail = Some(cover.clone());
            state.thumbnail_hash = hash_thumbnail_bytes(cover);
        }
        let _ = info_tx.send(state.clone());
        return;
    }

    if !text_matches(&metadata.title, &state.title) {
        log::debug!(
            "已丢弃不匹配 MusicData: {:?} {} - {}，当前曲目: {} - {}",
            metadata.track_id,
            metadata.title,
            metadata.artist,
            state.title,
            state.artist
        );
        return;
    }
    if !artist_matches(&metadata.artist, &metadata.artists, &state.artist) {
        log::debug!(
            "已丢弃歌手不匹配 MusicData: {:?} {} - {}，当前曲目: {} - {}",
            metadata.track_id,
            metadata.title,
            metadata.artist,
            state.title,
            state.artist
        );
        return;
    }

    state.lyrics = Some(music_data.lyrics);

    if let Some(ref cover) = metadata.cover
        && cover.len() > 4
        && (cover.starts_with(&[0xFF, 0xD8, 0xFF])
            || cover.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
            || cover.starts_with(b"GIF87a")
            || cover.starts_with(b"GIF89a")
            || (cover.len() >= 12 && cover.starts_with(b"RIFF") && &cover[8..12] == b"WEBP"))
    {
        let hash = hash_thumbnail_bytes(cover);
        if state.thumbnail_hash != hash {
            state.thumbnail = Some(cover.clone());
            state.thumbnail_hash = hash;
        }
    }

    let _ = info_tx.send(state.clone());
}
