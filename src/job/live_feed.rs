use std::collections::HashMap;

use futures::SinkExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// A line to be sent to the live console feed, tagged with a step ID.
struct FeedLine {
    step_id: String,
    content: String,
}

/// Handle for sending live console lines. Cheaply cloneable.
#[derive(Clone)]
pub struct FeedSender {
    tx: mpsc::Sender<FeedLine>,
}

impl FeedSender {
    pub async fn send(&self, step_id: &str, content: &str) {
        let line = FeedLine {
            step_id: step_id.to_string(),
            content: truncate_line(content, 1024).to_string(),
        };
        let _ = self.tx.send(line).await;
    }
}

/// Manages the WebSocket connection to GitHub's live console feed.
pub struct LiveFeed {
    sender: FeedSender,
    handle: JoinHandle<()>,
}

impl LiveFeed {
    /// Connect to the feed stream URL and start the background sender task.
    pub async fn connect(feed_url: &str, access_token: &str) -> Option<Self> {
        use tokio_tungstenite::tungstenite::http;

        // The FeedStreamUrl comes as https:// — convert to wss:// for WebSocket
        let ws_url = to_websocket_url(feed_url);

        let uri: http::Uri = match ws_url.parse() {
            Ok(u) => u,
            Err(e) => {
                warn!(error = %e, "invalid live feed URL");
                return None;
            }
        };

        let host = match (uri.host(), uri.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            _ => {
                warn!("live feed URL has no host");
                return None;
            }
        };

        let request = match http::Request::builder()
            .uri(&uri)
            .header("Host", &host)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header(
                "Sec-WebSocket-Key",
                tokio_tungstenite::tungstenite::handshake::client::generate_key(),
            )
            .body(())
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "failed to build live feed WebSocket request");
                return None;
            }
        };

        let ws_stream = match tokio_tungstenite::connect_async(request).await {
            Ok((stream, _)) => {
                debug!("live console feed WebSocket connected");
                stream
            }
            Err(e) => {
                warn!(error = %e, "failed to connect live console feed WebSocket");
                return None;
            }
        };

        let (tx, rx) = mpsc::channel::<FeedLine>(1024);
        let sender = FeedSender { tx };
        let handle = tokio::spawn(feed_task(ws_stream, rx));

        Some(Self { sender, handle })
    }

    pub fn sender(&self) -> &FeedSender {
        &self.sender
    }

    /// Shut down the live feed gracefully.
    pub async fn close(self) {
        drop(self.sender);
        let _ = self.handle.await;
    }
}

/// Background task that batches lines and sends them over the WebSocket.
async fn feed_task(
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    mut rx: mpsc::Receiver<FeedLine>,
) {
    let (mut ws_sink, ws_read) = futures::StreamExt::split(ws_stream);

    // Spawn a task to drain the read side — this ensures ping frames from the
    // server are received and pong responses are sent back automatically by
    // the tungstenite codec, keeping the connection alive.
    let read_handle = tokio::spawn(async move {
        use futures::StreamExt;
        let mut ws_read = ws_read;
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                Err(e) => {
                    debug!(error = %e, "live feed read error");
                    break;
                }
                _ => {}
            }
        }
    });

    // pending: step_id -> (buffered lines, next start_line index)
    let mut pending: HashMap<String, (Vec<String>, i64)> = HashMap::new();
    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));
    interval.tick().await;
    let mut dead = false;

    loop {
        tokio::select! {
            maybe_line = rx.recv() => {
                match maybe_line {
                    Some(line) => {
                        if !dead {
                            let entry = pending.entry(line.step_id).or_insert_with(|| (Vec::new(), 0));
                            entry.0.push(line.content);
                        }
                    }
                    None => {
                        // Channel closed — flush remaining and exit
                        if !dead {
                            for (step_id, (lines, start_line)) in pending.drain() {
                                if !lines.is_empty() {
                                    send_batch(&mut ws_sink, &step_id, &lines, start_line).await;
                                }
                            }
                            let _ = ws_sink.close().await;
                        }
                        read_handle.abort();
                        return;
                    }
                }
            }
            _ = interval.tick() => {
                if dead {
                    continue;
                }
                for (step_id, (lines, start_line)) in &mut pending {
                    if !lines.is_empty() {
                        let batch = std::mem::take(lines);
                        let count = batch.len() as i64;
                        if !send_batch(&mut ws_sink, step_id, &batch, *start_line).await {
                            warn!("live console feed connection lost, dropping further messages");
                            dead = true;
                            break;
                        }
                        *start_line += count;
                    }
                }
            }
        }
    }
}

/// Returns `true` if all chunks were sent successfully, `false` if the connection is dead.
async fn send_batch<S>(ws_sink: &mut S, step_id: &str, lines: &[String], start_line: i64) -> bool
where
    S: futures::Sink<tokio_tungstenite::tungstenite::Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let mut offset = 0i64;
    for chunk in lines.chunks(100) {
        let msg = serde_json::json!({
            "Count": chunk.len(),
            "Value": chunk,
            "StepId": step_id,
            "StartLine": start_line + offset + 1,
        });
        offset += chunk.len() as i64;

        if let Err(e) = ws_sink
            .send(tokio_tungstenite::tungstenite::Message::Text(
                msg.to_string().into(),
            ))
            .await
        {
            debug!(error = %e, "live console feed send failed");
            return false;
        }
    }
    true
}

fn truncate_line(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Convert an HTTPS URL to WSS for WebSocket connection.
fn to_websocket_url(url: &str) -> String {
    url.replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1)
}

#[cfg(test)]
#[path = "live_feed_test.rs"]
mod live_feed_test;
