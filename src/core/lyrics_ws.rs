use crate::core::config::APP_HOMEPAGE;
use crate::core::lyrics::{MusicData, parse_music_data_payload};
use crate::core::persistence;
use crate::utils::font::FontManager;
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum LyricsWsCommand {
    RequestTrackLyrics,
    Seek(u64),
    Play,
    Pause,
    TogglePlay,
    Next,
    Prev,
    GetPlaybackState,
    RequestMusicData,
    ConfigSnapshot,
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
}

#[derive(Clone)]
pub struct LyricsWsHandle {
    command_tx: broadcast::Sender<LyricsWsCommand>,
}

#[allow(dead_code)]
impl LyricsWsHandle {
    pub fn request_track_lyrics(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::RequestTrackLyrics);
    }

    pub fn seek(&self, position_ms: u64) {
        let _ = self.command_tx.send(LyricsWsCommand::Seek(position_ms));
    }

    pub fn play(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::Play);
    }

    pub fn pause(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::Pause);
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

    pub fn get_playback_state(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::GetPlaybackState);
    }

    pub fn request_music_data(&self) {
        let _ = self.command_tx.send(LyricsWsCommand::RequestMusicData);
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
    let listener = match TcpListener::bind("127.0.0.1:17195").await {
        Ok(listener) => listener,
        Err(err) => {
            log::error!("歌词 WebSocket 服务启动失败: {}", err);
            return;
        }
    };

    log::info!("歌词 WebSocket 服务已监听 ws://127.0.0.1:17195");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => {
                let Ok((stream, addr)) = accepted else {
                    continue;
                };
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
                    Ok(LyricsWsCommand::Play) => {
                        send_command(&mut ws_write, "play", None).await?;
                    }
                    Ok(LyricsWsCommand::Pause) => {
                        send_command(&mut ws_write, "pause", None).await?;
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
                    Ok(LyricsWsCommand::GetPlaybackState) => {
                        send_command(&mut ws_write, "get_playback_state", None).await?;
                    }
                    Ok(LyricsWsCommand::RequestMusicData) => {
                        send_command(&mut ws_write, "request_MusicData", None).await?;
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
        _ => {}
    }

    Ok(())
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
