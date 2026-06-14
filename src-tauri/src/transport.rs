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
    let (mut ws, _resp) = connect_async(&cfg.aavaaz_url)
        .await
        .with_context(|| format!("connect {}", cfg.aavaaz_url))?;

    let uid = uuid_v4();
    let hotwords = cfg.hotwords.join(",");
    let handshake = HandshakeMsg {
        uid: &uid,
        language: cfg.language.as_deref(),
        task: "transcribe",
        model: &cfg.model,
        use_vad: true,
        hotwords: &hotwords,
    };
    ws.send(Message::Text(serde_json::to_string(&handshake)?))
        .await?;

    loop {
        tokio::select! {
            chunk = audio_rx.recv() => {
                let Some(chunk) = chunk else { break };
                let mut bytes = Vec::with_capacity(chunk.len() * 4);
                for s in chunk {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                if let Err(e) = ws.send(Message::Binary(bytes)).await {
                    tracing::warn!(error = ?e, "ws send failed");
                    break;
                }
            }
            msg = ws.next() => {
                let Some(msg) = msg else { break };
                match msg? {
                    Message::Text(t) => {
                        if let Ok(env) = serde_json::from_str::<ServerEnvelope>(&t) {
                            for seg in env.segments {
                                if seg.completed {
                                    let _ = seg_tx.send(seg);
                                }
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
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
