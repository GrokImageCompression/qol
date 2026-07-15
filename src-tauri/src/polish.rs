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
        // An empty completion (no choices, or blank content) would silently
        // drop the utterance. Keep the raw transcript instead; don't record
        // the empty result in the rolling context.
        Ok(out) if out.is_empty() => raw.to_string(),
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

/// Tone to steer polish with, from the user's configured profiles. With the
/// per-app toggle off (or no rule matching the focused app), falls back to the
/// configured default tone. A rule matches when any of its app tokens is a
/// case-insensitive substring of the focused app name.
fn resolve_tone(cfg: &PolishConfig, app: Option<&str>) -> String {
    if let (true, Some(name)) = (cfg.per_app_tone, app) {
        let name = name.to_ascii_lowercase();
        let matched = cfg.tone_profiles.iter().find(|p| {
            p.apps
                .iter()
                .any(|a| !a.is_empty() && name.contains(&a.to_ascii_lowercase()))
        });
        if let Some(p) = matched {
            return p.tone.clone();
        }
    }
    cfg.default_tone.clone()
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
    let tone_hint = resolve_tone(cfg, app);
    let system = build_system_prompt(&tone_hint, &prior);

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
    // Bound the round-trip so a hung endpoint can't wedge the collector on a
    // single segment. On timeout reqwest errors, falling through to the raw
    // transcript like any other polish failure.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("build polish http client")?;
    let mut req = client.post(&url).json(&body);

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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spin up a one-shot HTTP server that reads a full request then replies
    /// with `response`. Returns a `base_url` pointing at it. Draining the
    /// request body before replying avoids an RST truncating reqwest's read.
    async fn stub_openai(response: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = Vec::new();
            let mut tmp = [0u8; 1024];
            let header_end = loop {
                let n = match stream.read(&mut tmp).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => n,
                };
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
            let content_len = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < header_end + content_len {
                match stream.read(&mut tmp).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                }
            }
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
        });
        format!("http://{addr}/v1")
    }

    fn http_response(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn enabled_cfg(base_url: String) -> PolishConfig {
        PolishConfig {
            enabled: true,
            base_url,
            model: "test-model".into(),
            api_key_env: String::new(),
            ..crate::config::Config::default().polish
        }
    }

    #[tokio::test]
    async fn success_returns_model_output_and_records_context() {
        let body = r#"{"choices":[{"message":{"content":"  Cleaned up text.  "}}]}"#;
        let cfg = enabled_cfg(stub_openai(http_response("200 OK", body)).await);
        let ctx = PolishContext::new();
        let out = polish(&cfg, "um cleaned up text", None, &ctx).await;
        assert_eq!(out, "Cleaned up text.");
        assert_eq!(ctx.snapshot(), "Cleaned up text.");
        assert!(!ctx.is_disabled());
    }

    #[tokio::test]
    async fn http_error_falls_back_to_raw_and_disables_session() {
        let cfg = enabled_cfg(stub_openai(http_response("401 Unauthorized", "{}")).await);
        let ctx = PolishContext::new();
        let out = polish(&cfg, "raw words", None, &ctx).await;
        assert_eq!(
            out, "raw words",
            "an API failure must not drop the transcript"
        );
        assert!(
            ctx.is_disabled(),
            "one failure disables polish for the session"
        );
        assert!(
            ctx.snapshot().is_empty(),
            "failed polish must not pollute context"
        );
    }

    #[tokio::test]
    async fn empty_completion_falls_back_to_raw() {
        // Both an empty choices list and blank content must keep the utterance.
        for body in [
            r#"{"choices":[]}"#,
            r#"{"choices":[{"message":{"content":"   "}}]}"#,
        ] {
            let cfg = enabled_cfg(stub_openai(http_response("200 OK", body)).await);
            let ctx = PolishContext::new();
            let out = polish(&cfg, "keep this", None, &ctx).await;
            assert_eq!(out, "keep this", "empty completion must not drop text");
            assert!(
                ctx.snapshot().is_empty(),
                "empty output must not enter context"
            );
        }
    }

    #[tokio::test]
    async fn malformed_body_falls_back_to_raw() {
        let cfg = enabled_cfg(stub_openai(http_response("200 OK", "this is not json")).await);
        let ctx = PolishContext::new();
        let out = polish(&cfg, "keep me", None, &ctx).await;
        assert_eq!(out, "keep me");
        assert!(ctx.is_disabled());
    }

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
    fn per_app_tone_toggle_gates_adaptation() {
        let mut cfg = enabled_cfg("http://unused".into());
        cfg.per_app_tone = true;
        assert_eq!(resolve_tone(&cfg, Some("Slack")), "casual chat");
        cfg.per_app_tone = false;
        assert_eq!(
            resolve_tone(&cfg, Some("Slack")),
            "natural prose",
            "toggle off must ignore the focused app"
        );
    }

    #[test]
    fn tone_hint_per_app() {
        // Default profiles, matched by case-insensitive substring.
        let cfg = enabled_cfg("http://unused".into());
        assert_eq!(resolve_tone(&cfg, Some("Slack")), "casual chat");
        assert_eq!(
            resolve_tone(&cfg, Some("Thunderbird")),
            "professional email"
        );
        assert_eq!(
            resolve_tone(&cfg, Some("Code - OSS")),
            "terse, code-friendly"
        );
        assert_eq!(resolve_tone(&cfg, Some("Firefox")), "natural prose");
        assert_eq!(resolve_tone(&cfg, None), "natural prose");
    }

    #[test]
    fn tone_profiles_are_config_driven() {
        let mut cfg = enabled_cfg("http://unused".into());
        cfg.tone_profiles = vec![crate::config::ToneProfile {
            apps: vec!["obsidian".into()],
            tone: "terse notes".into(),
        }];
        cfg.default_tone = "plain".into();
        // Substring match, focus name carries window title suffix.
        assert_eq!(resolve_tone(&cfg, Some("Obsidian - vault")), "terse notes");
        // No rule matches -> configured default, not the old hardcoded one.
        assert_eq!(resolve_tone(&cfg, Some("Slack")), "plain");
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
            ..crate::config::Config::default().polish
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
            ..crate::config::Config::default().polish
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
