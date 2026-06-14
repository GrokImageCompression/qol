use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::PolishConfig;

#[derive(Serialize)]
struct AnthropicReq<'a> {
    model: &'a str,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMsg<'a>>,
}

#[derive(Serialize)]
struct AnthropicMsg<'a> {
    role: &'a str,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResp {
    content: Vec<AnthropicBlock>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

/// Take raw transcript text and rewrite it for the active app's tone.
/// Returns the original text on any failure — never blocks dictation.
pub async fn polish(cfg: &PolishConfig, raw: &str, app: Option<&str>) -> String {
    if !cfg.enabled || raw.trim().is_empty() {
        return raw.to_string();
    }
    match polish_inner(cfg, raw, app).await {
        Ok(out) => out,
        Err(e) => {
            tracing::warn!(error = ?e, "polish failed, falling back to raw");
            raw.to_string()
        }
    }
}

async fn polish_inner(cfg: &PolishConfig, raw: &str, app: Option<&str>) -> Result<String> {
    let api_key = std::env::var(&cfg.api_key_env)
        .with_context(|| format!("missing env {}", cfg.api_key_env))?;

    let tone_hint = match app.map(str::to_ascii_lowercase).as_deref() {
        Some("slack") | Some("discord") | Some("telegram") => "casual chat",
        Some("mail") | Some("thunderbird") | Some("outlook") | Some("gmail") => {
            "professional email"
        }
        Some("code") | Some("code - oss") | Some("vscode") | Some("zed") | Some("nvim") => {
            "terse, code-friendly"
        }
        _ => "natural prose",
    };

    let system = format!(
        "You clean up dictated speech. Remove filler words (um, uh, like, you know). \
         Fix punctuation and capitalization. Preserve meaning exactly. \
         Match this tone: {tone_hint}. Output ONLY the cleaned text, no preamble."
    );

    let body = AnthropicReq {
        model: &cfg.model,
        max_tokens: 1024,
        system,
        messages: vec![AnthropicMsg {
            role: "user",
            content: raw.to_string(),
        }],
    };

    let client = reqwest::Client::new();
    let resp: AnthropicResp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let text = resp
        .content
        .into_iter()
        .filter(|b| b.kind == "text")
        .map(|b| b.text)
        .collect::<String>();
    Ok(text.trim().to_string())
}
