use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::config::Config;

#[derive(Debug, Serialize)]
struct HandshakeMsg<'a> {
    uid: &'a str,
    language: Option<&'a str>,
    task: &'a str,
    model: &'a str,
    use_vad: bool,
    hotwords: &'a str,
    send_last_n_segments: u32,
}

#[derive(Debug, Deserialize)]
pub struct Segment {
    // Aavaaz emits these as strings; we'll surface them when streaming
    // injection grows word-level timestamps. Kept on the wire type so
    // deserialization stays lossless.
    #[allow(dead_code)]
    pub start: Option<String>,
    #[allow(dead_code)]
    pub end: Option<String>,
    pub text: String,
    #[serde(default)]
    pub completed: bool,
}

#[derive(Debug, Deserialize)]
struct ServerEnvelope {
    #[serde(default)]
    segments: Vec<Segment>,
}

/// Open a WebSocket to Aavaaz/WhisperLive, send the handshake, stream PCM frames
/// from `audio_rx`, emit completed segments on `seg_tx`.
pub async fn run_session(
    cfg: Config,
    mut audio_rx: mpsc::UnboundedReceiver<Vec<f32>>,
    seg_tx: mpsc::UnboundedSender<Segment>,
) -> Result<()> {
    tracing::info!(url = %cfg.aavaaz_url, model = %cfg.model, "transport: connecting");
    let (mut ws, _resp) = connect_async(&cfg.aavaaz_url)
        .await
        .with_context(|| format!("connect {}", cfg.aavaaz_url))?;
    tracing::info!("transport: WS connected, sending handshake");

    let uid = uuid_v4();
    let hotwords = cfg.hotwords.join(",");
    let handshake = HandshakeMsg {
        uid: &uid,
        language: cfg.language.as_deref(),
        task: "transcribe",
        model: &cfg.model,
        use_vad: true,
        hotwords: &hotwords,
        send_last_n_segments: 1,
    };
    ws.send(Message::Text(serde_json::to_string(&handshake)?))
        .await?;
    tracing::info!(uid = %uid, "transport: handshake sent, entering loop");

    let mut chunks_sent: u64 = 0;
    let mut text_msgs: u64 = 0;
    let mut segments_emitted: u64 = 0;
    let mut seen_completed: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();

    loop {
        tokio::select! {
            chunk = audio_rx.recv() => {
                let Some(chunk) = chunk else {
                    tracing::info!(chunks_sent, "transport: audio channel closed by sender");
                    break;
                };
                let mut bytes = Vec::with_capacity(chunk.len() * 4);
                for s in chunk {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                if let Err(e) = ws.send(Message::Binary(bytes)).await {
                    tracing::warn!(error = ?e, chunks_sent, "transport: ws send failed");
                    break;
                }
                chunks_sent += 1;
                if chunks_sent.is_multiple_of(25) {
                    tracing::debug!(chunks_sent, "transport: streaming");
                }
            }
            msg = ws.next() => {
                let Some(msg) = msg else {
                    tracing::info!("transport: ws stream ended");
                    break;
                };
                match msg? {
                    Message::Text(t) => {
                        text_msgs += 1;
                        let preview = &t[..t.len().min(200)];
                        tracing::debug!(text_msgs, preview = %preview, "transport: text msg");
                        if let Ok(env) = serde_json::from_str::<ServerEnvelope>(&t) {
                            for seg in env.segments {
                                if !seg.completed {
                                    continue;
                                }
                                let key = (
                                    seg.start.clone().unwrap_or_default(),
                                    seg.end.clone().unwrap_or_default(),
                                );
                                if !seen_completed.insert(key) {
                                    continue;
                                }
                                tracing::debug!(
                                    completed = seg.completed,
                                    text = %seg.text,
                                    "transport: parsed segment",
                                );
                                segments_emitted += 1;
                                let _ = seg_tx.send(seg);
                            }
                        }
                    }
                    Message::Close(c) => {
                        tracing::info!(close = ?c, "transport: ws closed by peer");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    tracing::info!(
        chunks_sent,
        text_msgs,
        segments_emitted,
        "transport: session loop exited"
    );
    Ok(())
}

fn uuid_v4() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("qol-{nanos:x}")
}
