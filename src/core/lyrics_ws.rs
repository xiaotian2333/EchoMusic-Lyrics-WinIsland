use std::time::Duration;

use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

use crate::core::config::{APP_HOMEPAGE, is_valid_color};
use crate::core::lyrics::{MusicData, parse_music_data_payload};
use crate::core::persistence;
use crate::utils::font::FontManager;

const LYRICS_WS_ADDR: &str = "127.0.0.1:17195";
const BIND_RETRY_INITIAL_MS: u64 = 200;
const BIND_RETRY_MAX_MS: u64 = 2_000;

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum LyricsWsCommand {
    RequestTrackLyrics,
    Seek(u64),
    TogglePlay,
    Next,
    Prev,
    SetFavorite {
        is_favorite: bool,
        track_id: Option<String>,
    },
    SetVolume(f32),
    GetPlaybackState,
    ShowMainWindow,
    ConfigSnapshot,
    SetPlayMode(String),
}

#[derive(Clone, Debug)]
pub enum PlayAction {
    Play,
    Pause,
    Seek,
    Next,
    Prev,
}

#[derive(Clone, Debug)]
pub enum LyricsWsEvent {
    Connected,
    Subscribe,
    MusicData(MusicData),
    PlaybackState {
        position_ms: u64,
        duration_ms: u64,
        is_playing: bool,
    },
    PlaybackAction {
        action: PlayAction,
        position_ms: u64,
    },
    FavoriteState {
        is_favorite: bool,
        track_id: Option<String>,
    },
    VolumeState {
        volume: f32,
    },
    ShowMainWindow,
    PlayModeState {
        mode: String,
    },
    PluginDisabled,
}

#[derive(Clone)]
pub struct LyricsWsHandle {
    command_tx: broadcast::Sender<LyricsWsCommand>,
}

impl LyricsWsHandle {
    pub fn request_track_lyrics(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::RequestTrackLyrics);
    }

    pub fn seek(&self, position_ms: u64) {
        let _ = self.command_tx.send(LyricsWsCommand::Seek(position_ms));
    }

    pub fn toggle_play(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::TogglePlay);
    }

    pub fn next(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::Next);
    }

    pub fn prev(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::Prev);
    }

    pub fn set_favorite(&self, is_favorite: bool, track_id: Option<String>) {
        let _ = self.command_tx.send(LyricsWsCommand::SetFavorite {
            is_favorite,
            track_id,
        });
    }

    pub fn set_volume(&self, volume: f32) {
        if let Some(volume) = normalize_volume_value(volume as f64) {
            let _ = self.command_tx.send(LyricsWsCommand::SetVolume(volume));
        }
    }

    pub fn get_playback_state(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::GetPlaybackState);
    }

    pub fn show_main_window(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::ShowMainWindow);
    }

    pub fn set_play_mode(&self, mode: String) {
        let _ = self.command_tx.send(LyricsWsCommand::SetPlayMode(mode));
    }

    pub fn broadcast_config_snapshot(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::ConfigSnapshot);
    }
}

pub fn start_lyrics_ws_server(
    event_tx: mpsc::UnboundedSender<LyricsWsEvent>,
    cancel: CancellationToken,
) -> LyricsWsHandle {
    let (command_tx, _) = broadcast::channel(16);
    let server_command_tx = command_tx.clone();

    tokio::spawn(async move {
        run_server(event_tx, server_command_tx, cancel).await;
    });

    LyricsWsHandle { command_tx }
}

async fn run_server(
    event_tx: mpsc::UnboundedSender<LyricsWsEvent>,
    command_tx: broadcast::Sender<LyricsWsCommand>,
    cancel: CancellationToken,
) {
    let Some(listener) = bind_listener(&cancel).await else {
        return;
    };

    log::info!("歌词 WebSocket 服务已监听 ws://{}", LYRICS_WS_ADDR);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, addr)) = accepted else {
                    continue;
                };
                clear_stream_inherit(&stream);
                let client_event_tx = event_tx.clone();
                let command_rx = command_tx.subscribe();
                let client_command_tx = command_tx.clone();
                let client_cancel = cancel.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_client(stream, client_event_tx, client_command_tx, command_rx, client_cancel).await {
                        log::warn!("歌词 WebSocket 客户端 {} 已断开: {}", addr, err);
                    }
                });
            }
        }
    }
}

async fn bind_listener(cancel: &CancellationToken) -> Option<TcpListener> {
    let mut retry_delay = Duration::from_millis(BIND_RETRY_INITIAL_MS);

    loop {
        match TcpListener::bind(LYRICS_WS_ADDR).await {
            Ok(listener) => {
                clear_listener_inherit(&listener);
                return Some(listener);
            }
            Err(err) => {
                log::warn!(
                    "歌词 WebSocket 服务监听 {} 失败: {}，将在 {:?} 后重试",
                    LYRICS_WS_ADDR,
                    err,
                    retry_delay
                );
            }
        }

        tokio::select! {
            _ = cancel.cancelled() => return None,
            _ = tokio::time::sleep(retry_delay) => {}
        }

        retry_delay =
            Duration::from_millis((retry_delay.as_millis() as u64 * 2).min(BIND_RETRY_MAX_MS));
    }
}

#[cfg(windows)]
fn clear_listener_inherit(listener: &TcpListener) {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };

    let handle = HANDLE(listener.as_raw_socket() as *mut c_void);
    // SAFETY: raw socket 来自当前进程持有的 TcpListener，只修改 HANDLE_FLAG_INHERIT，
    // 不改变 socket 所有权，也不关闭或复制句柄。
    let result = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
    if let Err(err) = result {
        log::warn!("歌词 WebSocket 监听句柄继承标记清除失败: {}", err);
    }
}

#[cfg(not(windows))]
fn clear_listener_inherit(_listener: &TcpListener) {}

#[cfg(windows)]
fn clear_stream_inherit(stream: &tokio::net::TcpStream) {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Foundation::{
        HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, SetHandleInformation,
    };

    let handle = HANDLE(stream.as_raw_socket() as *mut c_void);
    // SAFETY: raw socket 来自当前进程持有的 TcpStream，只修改 HANDLE_FLAG_INHERIT，
    // 不改变 socket 所有权，也不关闭或复制句柄。
    let result = unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) };
    if let Err(err) = result {
        log::warn!("歌词 WebSocket 客户端句柄继承标记清除失败: {}", err);
    }
}

#[cfg(not(windows))]
fn clear_stream_inherit(_stream: &tokio::net::TcpStream) {}

async fn handle_client(
    stream: tokio::net::TcpStream,
    event_tx: mpsc::UnboundedSender<LyricsWsEvent>,
    command_tx: broadcast::Sender<LyricsWsCommand>,
    mut command_rx: broadcast::Receiver<LyricsWsCommand>,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws_stream = accept_async(stream).await?;
    let (mut ws_write, mut ws_read) = ws_stream.split();

    let _ = event_tx.send(LyricsWsEvent::Connected);
    send_request_track_lyrics(&mut ws_write).await?;
    // 连接后立即推送全量配置，使插件设置面板显示当前值
    send_config_snapshot(&mut ws_write).await?;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            command = command_rx.recv() => {
                match command {
                    Ok(LyricsWsCommand::RequestTrackLyrics) => {
                        send_command(&mut ws_write, "request_track_lyrics", None).await?;
                    }
                    Ok(LyricsWsCommand::Seek(position_ms)) => {
                        send_command(&mut ws_write, "seek", Some(json!({ "position_ms": position_ms }))).await?;
                    }
                    Ok(LyricsWsCommand::TogglePlay) => {
                        send_command(&mut ws_write, "toggle_play", None).await?;
                    }
                    Ok(LyricsWsCommand::Next) => {
                        send_command(&mut ws_write, "next", None).await?;
                    }
                    Ok(LyricsWsCommand::Prev) => {
                        send_command(&mut ws_write, "prev", None).await?;
                    }
                    Ok(LyricsWsCommand::SetFavorite {
                        is_favorite,
                        track_id,
                    }) => {
                        let data = if let Some(track_id) =
                            track_id.filter(|id| !id.trim().is_empty())
                        {
                            json!({ "id": track_id, "is_favorite": is_favorite })
                        } else {
                            json!({ "is_favorite": is_favorite })
                        };
                        send_command(&mut ws_write, "set_favorite", Some(data)).await?;
                    }
                    Ok(LyricsWsCommand::SetVolume(volume)) => {
                        send_command(&mut ws_write, "set_volume", Some(json!({ "volume": volume }))).await?;
                    }
                    Ok(LyricsWsCommand::GetPlaybackState) => {
                        send_command(&mut ws_write, "get_playback_state", None).await?;
                    }
                    Ok(LyricsWsCommand::ShowMainWindow) => {
                        send_command(&mut ws_write, "show_main_window", None).await?;
                    }
                    Ok(LyricsWsCommand::SetPlayMode(mode)) => {
                        send_command(
                            &mut ws_write,
                            "set_play_mode",
                            Some(json!({ "mode": mode })),
                        )
                        .await?;
                    }
                    Ok(LyricsWsCommand::ConfigSnapshot) => {
                        send_config_snapshot(&mut ws_write).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            message = ws_read.next() => {
                let Some(message) = message else {
                    break;
                };
                let message = message?;
                if message.is_close() {
                    break;
                }
                if message.is_text() {
                    handle_text_message(message.to_text()?, &event_tx, &command_tx, &mut ws_write).await?;
                }
            }
        }
    }

    Ok(())
}

async fn handle_text_message<S>(
    text: &str,
    event_tx: &mpsc::UnboundedSender<LyricsWsEvent>,
    command_tx: &broadcast::Sender<LyricsWsCommand>,
    ws_write: &mut S,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Sink<Message> + Unpin,
    <S as Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return Ok(());
    };

    let Some(message_type) = message.get("type").and_then(|v| v.as_str()) else {
        return Ok(());
    };

    match message_type {
        "ping" => {
            ws_write
                .send(Message::Text(json!({ "type": "pong" }).to_string().into()))
                .await?;
        }
        "subscribe" => {
            let _ = event_tx.send(LyricsWsEvent::Subscribe);
        }
        "MusicData" => {
            if let Some(payload) = message.get("payload")
                && let Some(music_data) = parse_music_data_payload(payload)
            {
                let _ = event_tx.send(LyricsWsEvent::MusicData(music_data));
            }
        }
        "config_update" => {
            let path = message
                .get("payload")
                .and_then(|p| p.get("path").and_then(|v| v.as_str()));
            let value = message.get("payload").and_then(|p| p.get("value"));
            if let (Some(path), Some(value)) = (path, value)
                && path != "custom_font_path"
            {
                apply_config_field(path, value);
                let _ = command_tx.send(LyricsWsCommand::ConfigSnapshot);
            }
        }
        "command" => {
            if message.get("source").and_then(|v| v.as_str()) != Some("plugin") {
                return Ok(());
            }
            if let Some(payload) = message.get("payload") {
                match payload.get("action").and_then(|v| v.as_str()) {
                    Some("get_config") => {
                        send_config_snapshot(ws_write).await?;
                    }
                    Some("open_font_picker") => {
                        let path = tokio::task::spawn_blocking(|| {
                            rfd::FileDialog::new()
                                .add_filter("Fonts", &["ttf", "otf"])
                                .pick_file()
                                .map(|p| p.to_string_lossy().into_owned())
                        })
                        .await
                        .unwrap_or(None);
                        if let Some(path) = path {
                            if !crate::utils::font::can_load_font_file(&path) {
                                log::warn!("无法加载所选字体文件: {}", path);
                                return Ok(());
                            }
                            apply_config_field("custom_font_path", &Value::String(path));
                            FontManager::global().refresh_custom_font();
                            let _ = command_tx.send(LyricsWsCommand::ConfigSnapshot);
                        }
                    }
                    Some("check_updates_now") => {
                        crate::utils::updater::check_for_updates_now();
                    }
                    Some("open_homepage") => {
                        let _ = open::that(APP_HOMEPAGE);
                    }
                    Some("open_settings") => {
                        if let Ok(exe) = std::env::current_exe() {
                            let _ = std::process::Command::new(exe).arg("--settings").spawn();
                        }
                    }
                    Some("disabled") => {
                        let _ = event_tx.send(LyricsWsEvent::PluginDisabled);
                    }
                    _ => {
                        handle_plugin_command(payload, event_tx).await?;
                    }
                }
            }
        }
        "track_lyrics" => {
            log::debug!("已忽略旧歌词事件 track_lyrics，请使用 MusicData");
        }
        "lyrics" => {
            log::debug!("已忽略旧歌词事件 lyrics，请使用 MusicData");
        }
        _ => {}
    }

    Ok(())
}

async fn handle_plugin_command(
    payload: &Value,
    event_tx: &mpsc::UnboundedSender<LyricsWsEvent>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let action = match payload.get("action").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return Ok(()),
    };

    let position_ms = payload
        .get("data")
        .and_then(|d| d.get("position_ms"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    match action {
        "play" => {
            let _ = event_tx.send(LyricsWsEvent::PlaybackAction {
                action: PlayAction::Play,
                position_ms,
            });
        }
        "pause" => {
            let _ = event_tx.send(LyricsWsEvent::PlaybackAction {
                action: PlayAction::Pause,
                position_ms,
            });
        }
        "seek" => {
            let _ = event_tx.send(LyricsWsEvent::PlaybackAction {
                action: PlayAction::Seek,
                position_ms,
            });
        }
        "next" => {
            let _ = event_tx.send(LyricsWsEvent::PlaybackAction {
                action: PlayAction::Next,
                position_ms: 0,
            });
        }
        "prev" => {
            let _ = event_tx.send(LyricsWsEvent::PlaybackAction {
                action: PlayAction::Prev,
                position_ms: 0,
            });
        }
        "position" => {
            let data = match payload.get("data") {
                Some(d) => d,
                None => return Ok(()),
            };
            let pos = data
                .get("position_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let dur = data
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let playing = data
                .get("is_playing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let _ = event_tx.send(LyricsWsEvent::PlaybackState {
                position_ms: pos,
                duration_ms: dur,
                is_playing: playing,
            });
        }
        "set_favorite" => {
            let data = match payload.get("data") {
                Some(d) => d,
                None => return Ok(()),
            };
            let Some(is_favorite) = data.get("is_favorite").and_then(|v| v.as_bool()) else {
                return Ok(());
            };
            let track_id = data
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let _ = event_tx.send(LyricsWsEvent::FavoriteState {
                is_favorite,
                track_id,
            });
        }
        "set_volume" => {
            let Some(volume) = payload
                .get("data")
                .and_then(|d| d.get("volume"))
                .and_then(|v| v.as_f64())
                .and_then(normalize_volume_value)
            else {
                return Ok(());
            };
            let _ = event_tx.send(LyricsWsEvent::VolumeState { volume });
        }
        "show_main_window" => {
            let _ = event_tx.send(LyricsWsEvent::ShowMainWindow);
        }
        "set_play_mode" => {
            let mode = payload
                .get("data")
                .and_then(|d| d.get("mode"))
                .and_then(|v| v.as_str())
                .unwrap_or("list")
                .to_string();
            let _ = event_tx.send(LyricsWsEvent::PlayModeState { mode });
        }
        _ => {}
    }

    Ok(())
}

pub(crate) fn normalize_volume_value(volume: f64) -> Option<f32> {
    if !volume.is_finite() {
        return None;
    }
    let rounded = (volume.clamp(0.0, 100.0) * 1000.0).round() / 1000.0;
    Some(rounded as f32)
}

async fn send_request_track_lyrics<S>(
    ws_write: &mut S,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Sink<Message> + Unpin,
    <S as Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    send_command(ws_write, "request_track_lyrics", None).await
}

async fn send_config_snapshot<S>(
    ws_write: &mut S,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Sink<Message> + Unpin,
    <S as Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let config = persistence::load_config();
    if let Ok(payload) = serde_json::to_value(&config) {
        let message = json!({
            "type": "config_snapshot",
            "payload": payload
        });
        ws_write
            .send(Message::Text(message.to_string().into()))
            .await?;
    }
    Ok(())
}

fn apply_config_field(path: &str, value: &Value) {
    if matches!(
        path,
        "lyrics_char_color_unplayed" | "lyrics_char_color_played"
    ) && !value.as_str().is_some_and(is_valid_color)
    {
        return;
    }
    let config = persistence::load_config();
    if let Ok(mut val) = serde_json::to_value(&config)
        && let Some(obj) = val.as_object_mut()
    {
        obj.insert(path.to_string(), value.clone());
        if let Ok(new_config) = serde_json::from_value(val) {
            persistence::save_config(&new_config);
        }
    }
}

async fn send_command<S>(
    ws_write: &mut S,
    action: &str,
    data: Option<Value>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    S: Sink<Message> + Unpin,
    <S as Sink<Message>>::Error: std::error::Error + Send + Sync + 'static,
{
    let mut payload = json!({ "action": action });
    if let Some(data) = data
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("data".to_string(), data);
    }
    let message = json!({
        "type": "command",
        "source": "server",
        "payload": payload
    });
    ws_write
        .send(Message::Text(message.to_string().into()))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_volume_value_clamps_and_rounds() {
        assert_eq!(normalize_volume_value(-0.2), Some(0.0));
        assert_eq!(normalize_volume_value(120.0), Some(100.0));
        assert_eq!(normalize_volume_value(80.1234), Some(80.123));
        assert_eq!(normalize_volume_value(80.1235), Some(80.124));
    }

    #[test]
    fn normalize_volume_value_rejects_invalid_number() {
        assert_eq!(normalize_volume_value(f64::NAN), None);
        assert_eq!(normalize_volume_value(f64::INFINITY), None);
    }
}
