use crate::core::lyrics::{TrackLyrics, parse_track_lyrics_payload};
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum LyricsWsCommand {
    RequestTrackLyrics,
    Seek(u64),
}

#[derive(Clone, Debug)]
pub enum LyricsWsEvent {
    Connected,
    Subscribe,
    TrackLyrics(TrackLyrics),
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
                let client_cancel = cancel.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_client(stream, client_event_tx, command_rx, client_cancel).await {
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
    mut command_rx: broadcast::Receiver<LyricsWsCommand>,
    cancel: CancellationToken,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws_stream = accept_async(stream).await?;
    let (mut ws_write, mut ws_read) = ws_stream.split();

    let _ = event_tx.send(LyricsWsEvent::Connected);
    send_request_track_lyrics(&mut ws_write).await?;

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
                    handle_text_message(message.to_text()?, &event_tx, &mut ws_write).await?;
                }
            }
        }
    }

    Ok(())
}

async fn handle_text_message<S>(
    text: &str,
    event_tx: &mpsc::UnboundedSender<LyricsWsEvent>,
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
        "track_lyrics" => {
            if let Some(payload) = message.get("payload")
                && let Some(track_lyrics) = parse_track_lyrics_payload(payload)
            {
                let _ = event_tx.send(LyricsWsEvent::TrackLyrics(track_lyrics));
            }
        }
        "lyrics" => {
            log::debug!("已忽略旧歌词事件 lyrics，请使用 track_lyrics");
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
        "payload": payload
    });
    ws_write
        .send(Message::Text(message.to_string().into()))
        .await?;
    Ok(())
}
