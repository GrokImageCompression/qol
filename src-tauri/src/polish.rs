//! LLM polish pass via the OpenAI-compatible Chat Completions API.
//!
//! Works against any provider that speaks the `/v1/chat/completions` shape —
//! OpenAI, Groq, OpenRouter, Together, Cerebras, Mistral, local Ollama,
//! llama.cpp's server, vLLM. Configured by `base_url`, `model`, and an
//! optional env var holding a bearer token.
//!
//! To keep tone consistent across a long dictation we maintain a rolling
//! window of previously-polished output and include it in the system prompt
//! so the model continues in the same register.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::PolishConfig;

const CONTEXT_CHAR_CAP: usize = 2000;

#[derive(Serialize)]
struct ChatReq<'a> {
    model: &'a str,
    messages: Vec<ChatMsg<'a>>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ChatMsg<'a> {
    role: &'a str,
    content: String,
}

#[derive(Deserialize)]
struct ChatResp {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: RespMsg,
}

#[derive(Deserialize)]
struct RespMsg {
    #[serde(default)]
    content: String,
}

/// Sliding window of previously-polished output for one dictation session.
///
/// Also carries a per-session "disabled" flag: if a polish call fails
/// (401 from the API, network error, malformed response, anything), we
/// flip the flag, log the failure once, and skip polish for the rest of
/// the session — the raw transcript still gets injected, but we stop
/// spamming the LLM endpoint with every segment. Fresh sessions start
/// re-enabled, so the user gets exactly one warning per session if their
/// config is broken.
#[derive(Clone, Default)]
pub struct PolishContext {
    inner: Arc<Mutex<String>>,
    disabled: Arc<AtomicBool>,
}

impl PolishContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> String {
        self.inner.lock().clone()
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Relaxed)
    }

    /// Mark this context as broken so subsequent polish() calls skip the
    /// network round-trip. Returns true if this was the first time we
    /// disabled it (caller uses that to log exactly once).
    pub fn disable(&self) -> bool {
        !self.disabled.swap(true, Ordering::Relaxed)
    }

    pub fn append(&self, polished: &str) {
        let mut g = self.inner.lock();
        if !g.is_empty() {
            g.push(' ');
        }
        g.push_str(polished.trim());
        if g.len() > CONTEXT_CHAR_CAP {
            let cut = g.len() - CONTEXT_CHAR_CAP;
            let trim_to = g[cut..]
                .find(char::is_whitespace)
                .map(|i| cut + i + 1)
                .unwrap_or(cut);
            *g = g[trim_to..].to_string();
        }
    }
}

pub async fn polish(
    cfg: &PolishConfig,
    raw: &str,
    app: Option<&str>,
    ctx: &PolishContext,
) -> String {
    if !cfg.enabled || raw.trim().is_empty() || ctx.is_disabled() {
        return raw.to_string();
    }
    match polish_inner(cfg, raw, app, ctx).await {
        Ok(out) => {
            ctx.append(&out);
            out
        }
        Err(e) => {
            // First failure flips the per-session disable flag. Subsequent
            // segments skip the network round-trip entirely — no spam.
            if ctx.disable() {
                tracing::warn!(
                    error = ?e,
                    "polish failed; disabling for the rest of this session. \
                     Check your base_url / api_key_env / model. \
                     Raw transcripts will keep flowing through."
                );
            }
            raw.to_string()
        }
    }
}

fn tone_hint_for(app: Option<&str>) -> &'static str {
    match app.map(str::to_ascii_lowercase).as_deref() {
        Some("slack") | Some("discord") | Some("telegram") => "casual chat",
        Some("mail") | Some("thunderbird") | Some("outlook") | Some("gmail") => {
            "professional email"
        }
        Some("code") | Some("code - oss") | Some("vscode") | Some("zed") | Some("nvim") => {
            "terse, code-friendly"
        }
        _ => "natural prose",
    }
}

fn build_system_prompt(tone_hint: &str, prior: &str) -> String {
    if prior.is_empty() {
        format!(
            "You clean up dictated speech. Remove filler words (um, uh, like, you know). \
             Fix punctuation and capitalization. Preserve meaning exactly. \
             Match this tone: {tone_hint}. Output ONLY the cleaned text, no preamble."
        )
    } else {
        format!(
            "You clean up dictated speech. Remove filler words (um, uh, like, you know). \
             Fix punctuation and capitalization. Preserve meaning exactly. \
             Match this tone: {tone_hint}. \
             Continue in the same style and register as the prior text below. \
             Do NOT repeat or summarize the prior text. \
             Output ONLY the cleaned version of the new utterance.\n\n\
             Prior text (for style continuity):\n{prior}"
        )
    }
}

async fn polish_inner(
    cfg: &PolishConfig,
    raw: &str,
    app: Option<&str>,
    ctx: &PolishContext,
) -> Result<String> {
    let prior = ctx.snapshot();
    let tone_hint = tone_hint_for(app);
    let system = build_system_prompt(tone_hint, &prior);

    let body = ChatReq {
        model: &cfg.model,
        messages: vec![
            ChatMsg {
                role: "system",
                content: system,
            },
            ChatMsg {
                role: "user",
                content: raw.to_string(),
            },
        ],
        temperature: 0.0,
        max_tokens: 1024,
    };

    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut req = reqwest::Client::new().post(&url).json(&body);

    // Bearer auth only if the configured env var is set. Lets local servers
    // (Ollama, llama.cpp) work with no key configured.
    if !cfg.api_key_env.is_empty() {
        if let Ok(key) = std::env::var(&cfg.api_key_env) {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }
    }

    let resp: ChatResp = req
        .send()
        .await
        .with_context(|| format!("POST {url}"))?
        .error_for_status()?
        .json()
        .await?;

    let text = resp
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .unwrap_or_default();
    Ok(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_starts_empty() {
        let ctx = PolishContext::new();
        assert!(ctx.snapshot().is_empty());
    }

    #[test]
    fn append_joins_with_space() {
        let ctx = PolishContext::new();
        ctx.append("Hello there.");
        ctx.append("How are you?");
        assert_eq!(ctx.snapshot(), "Hello there. How are you?");
    }

    #[test]
    fn append_trims_inner_whitespace() {
        let ctx = PolishContext::new();
        ctx.append("  one  ");
        ctx.append("\ttwo\n");
        assert_eq!(ctx.snapshot(), "one two");
    }

    #[test]
    fn cap_keeps_recent_text() {
        let ctx = PolishContext::new();
        for i in 0..30 {
            ctx.append(&format!("chunk-{i:02} ").repeat(10));
        }
        let snap = ctx.snapshot();
        assert!(
            snap.len() <= CONTEXT_CHAR_CAP + 200,
            "snap len {} exceeded cap {}",
            snap.len(),
            CONTEXT_CHAR_CAP
        );
        assert!(snap.contains("chunk-29"));
        assert!(!snap.contains("chunk-00"));
    }

    #[test]
    fn cap_cuts_at_word_boundary() {
        let ctx = PolishContext::new();
        ctx.append(&"alpha bravo charlie delta echo ".repeat(200));
        let snap = ctx.snapshot();
        let first_word = snap.split_whitespace().next().unwrap();
        assert!(
            ["alpha", "bravo", "charlie", "delta", "echo"].contains(&first_word),
            "got partial word {first_word:?}"
        );
    }

    #[test]
    fn tone_hint_per_app() {
        assert_eq!(tone_hint_for(Some("Slack")), "casual chat");
        assert_eq!(tone_hint_for(Some("Thunderbird")), "professional email");
        assert_eq!(tone_hint_for(Some("code - OSS")), "terse, code-friendly");
        assert_eq!(tone_hint_for(Some("Firefox")), "natural prose");
        assert_eq!(tone_hint_for(None), "natural prose");
    }

    #[test]
    fn system_prompt_includes_prior_when_present() {
        let s = build_system_prompt("natural prose", "Hello there.");
        assert!(s.contains("Hello there."));
        assert!(s.contains("Prior text"));
    }

    #[test]
    fn system_prompt_skips_prior_block_when_empty() {
        let s = build_system_prompt("natural prose", "");
        assert!(!s.contains("Prior text"));
    }

    #[test]
    fn disable_returns_true_only_once() {
        let ctx = PolishContext::new();
        assert!(!ctx.is_disabled());
        assert!(ctx.disable(), "first call should be the transition");
        assert!(ctx.is_disabled());
        assert!(!ctx.disable(), "second call must NOT log again");
        assert!(!ctx.disable(), "still no");
    }

    #[test]
    fn disabled_ctx_short_circuits_polish() {
        let cfg = PolishConfig {
            enabled: true, // enabled in config — but ctx flag should win
            base_url: "http://example.invalid".into(),
            model: "ignored".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            per_app_tone: true,
        };
        let ctx = PolishContext::new();
        ctx.disable();
        let out = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(polish(&cfg, "raw text", None, &ctx));
        // No HTTP call attempted; raw passes through, context stays empty.
        assert_eq!(out, "raw text");
        assert!(ctx.snapshot().is_empty());
    }

    #[test]
    fn disabled_polish_returns_raw_and_skips_context() {
        let cfg = PolishConfig {
            enabled: false,
            base_url: "http://invalid.invalid".into(),
            model: "doesnt-matter".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            per_app_tone: true,
        };
        let ctx = PolishContext::new();
        let out = tokio::runtime::Runtime::new().unwrap().block_on(polish(
            &cfg,
            "raw text here",
            None,
            &ctx,
        ));
        assert_eq!(out, "raw text here");
        assert!(ctx.snapshot().is_empty());
    }
}
