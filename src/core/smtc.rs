use crate::core::lyrics::{LyricLine, TrackLyrics, current_lyric_index};
use crate::core::lyrics_ws::{LyricsWsEvent, LyricsWsHandle, start_lyrics_ws_server};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

pub const TARGET_MEDIA_APP_ID: &str = "com.hoowhoami.echomusic";

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

#[derive(Debug, Clone)]
enum PlaybackCommand {
    Toggle,
    Next,
    Prev,
}

pub struct SmtcListener {
    info_rx: watch::Receiver<MediaInfo>,
    seek_tx: mpsc::UnboundedSender<u64>,
    playback_tx: mpsc::UnboundedSender<PlaybackCommand>,
    lyrics_ws_handle: LyricsWsHandle,
    cancel_token: CancellationToken,
}

impl SmtcListener {
    pub fn new() -> Self {
        let (info_tx, info_rx) = watch::channel(MediaInfo::default());
        let (seek_tx, seek_rx) = mpsc::unbounded_channel();
        let (playback_tx, playback_rx) = mpsc::unbounded_channel();
        let (lyrics_event_tx, lyrics_event_rx) = mpsc::unbounded_channel();
        let cancel_token = CancellationToken::new();
        let lyrics_ws_handle = start_lyrics_ws_server(lyrics_event_tx, cancel_token.clone());

        let cancel = cancel_token.clone();
        let poll_lyrics_ws_handle = lyrics_ws_handle.clone();
        tokio::task::spawn_blocking(move || {
            smtc_poll_loop(
                info_tx,
                seek_rx,
                playback_rx,
                lyrics_event_rx,
                poll_lyrics_ws_handle,
                cancel,
            );
        });

        Self {
            info_rx,
            seek_tx,
            playback_tx,
            lyrics_ws_handle,
            cancel_token,
        }
    }

    pub fn get_info(&self) -> MediaInfo {
        self.info_rx.borrow().clone()
    }

    pub fn request_seek(&self, position_ms: u64) {
        let _ = self.seek_tx.send(position_ms);
        self.lyrics_ws_handle.seek(position_ms);
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

impl Drop for SmtcListener {
    fn drop(&mut self) {
        self.cancel_token.cancel();
    }
}

#[allow(clippy::too_many_arguments)]
fn smtc_poll_loop(
    info_tx: watch::Sender<MediaInfo>,
    mut seek_rx: mpsc::UnboundedReceiver<u64>,
    mut playback_rx: mpsc::UnboundedReceiver<PlaybackCommand>,
    mut lyrics_event_rx: mpsc::UnboundedReceiver<LyricsWsEvent>,
    lyrics_ws_handle: LyricsWsHandle,
    cancel: CancellationToken,
) {
    // SAFETY: CoInitializeEx initializes COM for this thread. We use
    // COINIT_MULTITHREADED because tokio's spawn_blocking pool is MTA.
    // If it fails (e.g. already initialized with a different mode), we
    // skip creating the guard so CoUninitialize is not called unbalanced.
    let com_initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
    struct ComGuard;
    impl Drop for ComGuard {
        fn drop(&mut self) {
            // SAFETY: CoUninitialize balances the successful CoInitializeEx
            // that triggered the creation of this guard.
            unsafe { CoUninitialize() };
        }
    }
    let _com_guard = com_initialized.then_some(ComGuard);

    let manager = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
        Ok(op) => match op.get() {
            Ok(m) => m,
            Err(_) => return,
        },
        Err(_) => return,
    };

    // COM event bridge: COM callback -> std::sync::mpsc -> polling loop
    let (event_tx, event_rx) = std::sync::mpsc::channel::<()>();
    let handler = TypedEventHandler::new(
        move |_m: &Option<GlobalSystemMediaTransportControlsSessionManager>, _| {
            let _ = event_tx.send(());
            Ok(())
        },
    );
    let _ = manager.SessionsChanged(&handler);

    let mut pending_seek: Option<(u64, Instant, Duration)> = None;

    // Initial update with retries for SMTC timeline readiness.
    // Some music apps (Spotify, Netease) take 1-2s to populate
    // TimelineProperties after session creation, so we retry up
    // to 2 seconds (10 × 200ms).
    for attempt in 0..10 {
        update_media_info(&manager, &info_tx, &lyrics_ws_handle, &mut pending_seek);
        let info = info_tx.borrow();
        let timeline_ready = info.duration_ms > 0
            || info.position_ms > 0
            || !info.is_playing
            || info.title.is_empty();
        if timeline_ready {
            drop(info);
            break;
        }
        drop(info);
        if attempt < 9 {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    let mut last_manager_refresh = Instant::now();
    let mut current_manager = manager;
    let mut last_regular_update = Instant::now();

    while !cancel.is_cancelled() {
        // Refresh manager every 30 seconds
        if last_manager_refresh.elapsed() > Duration::from_secs(30) {
            if let Ok(new_mgr_op) = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                && let Ok(new_mgr) = new_mgr_op.get()
            {
                current_manager = new_mgr;
                let _ = current_manager.SessionsChanged(&handler);
            }
            last_manager_refresh = Instant::now();
        }

        // Drain WebSocket lyric events
        while let Ok(event) = lyrics_event_rx.try_recv() {
            match event {
                LyricsWsEvent::Connected | LyricsWsEvent::Subscribe => {
                    lyrics_ws_handle.request_track_lyrics();
                }
                LyricsWsEvent::TrackLyrics(track_lyrics) => {
                    apply_track_lyrics(&info_tx, track_lyrics);
                }
            }
        }

        // Handle seek request (keep only the latest)
        let mut seek_pos = None;
        while let Ok(v) = seek_rx.try_recv() {
            seek_pos = Some(v);
        }
        if let Some(seek_pos) = seek_pos
            && let Some(session) = get_target_session(&current_manager)
        {
            let ticks = seek_pos as i64 * 10_000;
            let _ = session.TryChangePlaybackPositionAsync(ticks);
            pending_seek = Some((seek_pos, Instant::now(), Duration::from_secs(5)));
            let mut info = info_tx.borrow().clone();
            info.position_ms = seek_pos;
            info.last_update = Instant::now();
            info.last_smtc_pos = seek_pos;
            let _ = info_tx.send(info);
        }

        // Handle playback commands
        while let Ok(cmd) = playback_rx.try_recv() {
            if let Some(session) = get_target_session(&current_manager) {
                match cmd {
                    PlaybackCommand::Toggle => {
                        if let Ok(pb_info) = session.GetPlaybackInfo()
                            && let Ok(status) = pb_info.PlaybackStatus()
                        {
                            if status == windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing {
                                    let _ = session.TryPauseAsync();
                                } else {
                                    let _ = session.TryPlayAsync();
                                }
                        }
                    }
                    PlaybackCommand::Next => {
                        let _ = session.TrySkipNextAsync();
                    }
                    PlaybackCommand::Prev => {
                        let _ = session.TrySkipPreviousAsync();
                    }
                }
            }
        }

        // Check COM events — when triggered, immediately update and reset the regular timer
        if event_rx.try_recv().is_ok() {
            update_media_info(
                &current_manager,
                &info_tx,
                &lyrics_ws_handle,
                &mut pending_seek,
            );
            last_regular_update = Instant::now();
        }

        // Regular update — only if last update was > 300ms ago
        if last_regular_update.elapsed() > Duration::from_millis(300) {
            update_media_info(
                &current_manager,
                &info_tx,
                &lyrics_ws_handle,
                &mut pending_seek,
            );
            last_regular_update = Instant::now();
        }

        std::thread::sleep(Duration::from_millis(300));
    }
}

fn update_media_info(
    manager: &GlobalSystemMediaTransportControlsSessionManager,
    info_tx: &watch::Sender<MediaInfo>,
    lyrics_ws_handle: &LyricsWsHandle,
    pending_seek: &mut Option<(u64, Instant, Duration)>,
) {
    if let Some(session) = get_target_session(manager) {
        let _ = fetch_properties(&session, info_tx, lyrics_ws_handle, pending_seek);
    } else {
        *pending_seek = None;
        let info = info_tx.borrow();
        if !info.title.is_empty() {
            drop(info);
            let _ = info_tx.send(MediaInfo::default());
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

fn artist_matches(track: &TrackLyrics, media_artist: &str) -> bool {
    if media_artist.trim().is_empty()
        || (track.artist.trim().is_empty() && track.artists.is_empty())
    {
        return true;
    }
    if text_matches(&track.artist, media_artist) {
        return true;
    }
    track
        .artists
        .iter()
        .any(|artist| text_matches(artist, media_artist))
}

fn apply_track_lyrics(info_tx: &watch::Sender<MediaInfo>, track_lyrics: TrackLyrics) {
    let current = info_tx.borrow();
    if current.title.trim().is_empty() {
        return;
    }
    if !text_matches(&track_lyrics.title, &current.title) {
        log::debug!(
            "已丢弃不匹配歌词: {:?} {} - {}，当前曲目: {} - {}",
            track_lyrics.track_id,
            track_lyrics.title,
            track_lyrics.artist,
            current.title,
            current.artist
        );
        return;
    }
    if !artist_matches(&track_lyrics, &current.artist) {
        log::debug!(
            "已丢弃歌手不匹配歌词: {:?} {} - {}，当前曲目: {} - {}",
            track_lyrics.track_id,
            track_lyrics.title,
            track_lyrics.artist,
            current.title,
            current.artist
        );
        return;
    }

    drop(current);
    let mut new_info = info_tx.borrow().clone();
    new_info.lyrics = Some(track_lyrics.lyrics);
    let _ = info_tx.send(new_info);
}

fn is_target_app_session(session: &GlobalSystemMediaTransportControlsSession) -> bool {
    session
        .SourceAppUserModelId()
        .map(|id| id == TARGET_MEDIA_APP_ID)
        .unwrap_or(false)
}

fn get_target_session(
    mgr: &GlobalSystemMediaTransportControlsSessionManager,
) -> Option<GlobalSystemMediaTransportControlsSession> {
    let mut audio_session = None;
    if let Ok(sessions) = mgr.GetSessions()
        && let Ok(count) = sessions.Size()
    {
        for i in 0..count {
            if let Ok(session) = sessions.GetAt(i) {
                if !is_target_app_session(&session) {
                    continue;
                }
                if !is_music_session(&session) {
                    continue;
                }
                if let Ok(pb_info) = session.GetPlaybackInfo()
                        && let Ok(status) = pb_info.PlaybackStatus()
                            && status == windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing {
                                return Some(session);
                            }
                if audio_session.is_none() {
                    audio_session = Some(session);
                }
            }
        }
    }
    if let Some(session) = audio_session {
        return Some(session);
    }
    if let Ok(session) = mgr.GetCurrentSession()
        && is_target_app_session(&session)
        && is_music_session(&session)
    {
        return Some(session);
    }
    None
}

fn is_music_session(session: &GlobalSystemMediaTransportControlsSession) -> bool {
    if let Ok(pb_info) = session.GetPlaybackInfo()
        && let Ok(playback_type) = pb_info.PlaybackType()
        && let Ok(value) = playback_type.Value()
        && value == windows::Media::MediaPlaybackType::Video
    {
        return false;
    }
    true
}

fn fetch_properties(
    session: &GlobalSystemMediaTransportControlsSession,
    info_tx: &watch::Sender<MediaInfo>,
    lyrics_ws_handle: &LyricsWsHandle,
    pending_seek: &mut Option<(u64, Instant, Duration)>,
) -> windows::core::Result<()> {
    if !is_music_session(session) {
        let info = info_tx.borrow();
        if !info.title.is_empty() {
            drop(info);
            let _ = info_tx.send(MediaInfo::default());
        }
        return Ok(());
    }

    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let pb_info = session.GetPlaybackInfo()?;
    let is_playing = pb_info.PlaybackStatus()? == windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;

    let smtc_pos = if let Ok(tl) = session.GetTimelineProperties() {
        if let Ok(pos) = tl.Position() {
            let raw = pos.Duration;
            if raw > 0 { (raw / 10_000) as u64 } else { 0 }
        } else {
            0
        }
    } else {
        0
    };

    let duration_secs = if let Ok(tl) = session.GetTimelineProperties() {
        if let Ok(end) = tl.EndTime() {
            let raw = end.Duration;
            if raw > 0 {
                (raw / 10_000_000) as u64
            } else {
                0
            }
        } else {
            0
        }
    } else {
        0
    };

    let duration_ms_from_tl = if let Ok(tl) = session.GetTimelineProperties() {
        if let Ok(end) = tl.EndTime() {
            let raw = end.Duration;
            if raw > 0 { (raw / 10_000) as u64 } else { 0 }
        } else {
            0
        }
    } else {
        0
    };

    let new_title = props.Title()?.to_string();
    let new_artist = props.Artist()?.to_string();
    let new_album = props.AlbumTitle()?.to_string();
    let mut should_request_lyrics = false;
    let mut should_fetch_thumbnail = false;

    {
        let mut info = info_tx.borrow().clone();
        let song_changed =
            info.title != new_title || info.artist != new_artist || info.album != new_album;
        if song_changed {
            *pending_seek = Some((0, Instant::now(), Duration::from_secs(7)));
            info.title = new_title.clone();
            info.artist = new_artist.clone();
            info.album = new_album.clone();
            info.duration_secs = duration_secs;
            info.duration_ms = duration_ms_from_tl;
            info.lyrics = None;
            info.thumbnail = None;
            info.thumbnail_hash = 0;
            info.position_ms = 0;
            info.last_smtc_pos = smtc_pos;
            info.last_update = Instant::now();
            info.last_thumbnail_fetch = Instant::now();
            should_request_lyrics = true;
            should_fetch_thumbnail = true;
        } else if (info.is_playing != is_playing
            && info.thumbnail.is_none()
            && !new_title.is_empty())
            || (!new_title.is_empty()
                && info.last_thumbnail_fetch.elapsed() >= Duration::from_secs(5))
        {
            info.last_thumbnail_fetch = Instant::now();
            should_fetch_thumbnail = true;
        }
        let smtc_changed = smtc_pos != info.last_smtc_pos;
        let suppress_smtc_sync =
            if let Some((target_pos, started_at, protect_duration)) = *pending_seek {
                let diff_with_target = (smtc_pos as i64 - target_pos as i64).abs();
                if target_pos == 0 && smtc_pos <= 1000 {
                    *pending_seek = None;
                    false
                } else if target_pos > 0 && diff_with_target <= 2000 {
                    *pending_seek = None;
                    false
                } else if started_at.elapsed() <= protect_duration {
                    true
                } else {
                    *pending_seek = None;
                    false
                }
            } else {
                false
            };

        let should_sync = !song_changed
            && !suppress_smtc_sync
            && ((info.is_playing != is_playing)
                || (smtc_pos > 0 && info.position_ms == 0)
                || smtc_changed);

        if should_sync {
            if smtc_pos > 0 || !song_changed {
                info.position_ms = smtc_pos;
            }
            info.last_update = Instant::now();
        }

        info.last_smtc_pos = smtc_pos;
        info.is_playing = is_playing;
        info.duration_secs = duration_secs;
        info.duration_ms = duration_ms_from_tl;
        let _ = info_tx.send(info);
    }

    if should_fetch_thumbnail {
        let info_tx_clone = info_tx.clone();
        let session_clone = session.clone();
        let title_clone = new_title.clone();
        let artist_clone = new_artist.clone();
        let is_song_change = should_request_lyrics;
        tokio::task::spawn_blocking(move || {
            if is_song_change {
                std::thread::sleep(Duration::from_millis(800));
            }
            for attempt in 0..10 {
                let res = (|| -> windows::core::Result<(String, String, Vec<u8>)> {
                    let props = session_clone.TryGetMediaPropertiesAsync()?.get()?;
                    let fetched_title = props.Title()?.to_string();
                    let fetched_artist = props.Artist()?.to_string();
                    if fetched_title != title_clone || fetched_artist != artist_clone {
                        // HRESULT(-2) is a sentinel value to signal stale media properties,
                        // not a standard COM error code. The caller retries on this error.
                        return Err(windows::core::Error::new(
                            windows::core::HRESULT(-2),
                            "Stale properties",
                        ));
                    }
                    let thumb_ref = props.Thumbnail()?;
                    let stream = thumb_ref.OpenReadAsync()?.get()?;
                    let size = stream.Size()?;
                    if size == 0 {
                        return Err(windows::core::Error::new(
                            windows::core::HRESULT(-1),
                            "Empty thumbnail",
                        ));
                    }
                    let buffer = windows::Storage::Streams::Buffer::Create(size as u32)?;
                    let res_buffer = stream
                        .ReadAsync(
                            &buffer,
                            size as u32,
                            windows::Storage::Streams::InputStreamOptions::None,
                        )?
                        .get()?;
                    let reader = windows::Storage::Streams::DataReader::FromBuffer(&res_buffer)?;
                    let mut bytes = vec![0u8; size as usize];
                    reader.ReadBytes(&mut bytes)?;
                    Ok((fetched_title, fetched_artist, bytes))
                })();

                if let Ok((_t, _a, bytes)) = res {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut hasher = DefaultHasher::new();
                    bytes.hash(&mut hasher);
                    let hash = hasher.finish();

                    let current = info_tx_clone.borrow();
                    if current.title == title_clone
                        && current.artist == artist_clone
                        && current.thumbnail_hash != hash
                    {
                        drop(current);
                        let mut new_info = info_tx_clone.borrow().clone();
                        new_info.thumbnail = Some(Arc::new(bytes));
                        new_info.thumbnail_hash = hash;
                        let _ = info_tx_clone.send(new_info);
                    }
                    return;
                }
                let delay = if attempt < 3 { 300 } else { 500 };
                std::thread::sleep(Duration::from_millis(delay));
            }
        });
    }

    if should_request_lyrics {
        lyrics_ws_handle.request_track_lyrics();
    }
    Ok(())
}
