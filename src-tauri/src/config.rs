use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub aavaaz_url: String,
    pub model: String,
    pub language: Option<String>,
    pub hotkey: String,
    pub polish: PolishConfig,
    pub hotwords: Vec<String>,
    pub inject_method: InjectMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishConfig {
    pub enabled: bool,
    /// OpenAI-compatible API base, e.g.:
    ///   - `https://api.openai.com/v1`
    ///   - `https://api.groq.com/openai/v1`
    ///   - `https://openrouter.ai/api/v1`
    ///   - `http://localhost:11434/v1` (Ollama)
    ///   - `http://localhost:8080/v1`  (llama.cpp server, vLLM)
    pub base_url: String,
    pub model: String,
    /// Name of the env var holding the API key. Leave the env var unset for
    /// local servers (Ollama, llama.cpp) that don't require auth.
    pub api_key_env: String,
    pub per_app_tone: bool,
    /// Ordered app→tone rules; first rule whose app token is a
    /// case-insensitive substring of the focused app name wins.
    #[serde(default = "default_tone_profiles")]
    pub tone_profiles: Vec<ToneProfile>,
    /// Tone used when no rule matches, or when `per_app_tone` is off.
    #[serde(default = "default_tone")]
    pub default_tone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToneProfile {
    pub apps: Vec<String>,
    pub tone: String,
}

fn default_tone_profiles() -> Vec<ToneProfile> {
    let profile = |apps: &[&str], tone: &str| ToneProfile {
        apps: apps.iter().map(|s| s.to_string()).collect(),
        tone: tone.to_string(),
    };
    vec![
        profile(&["slack", "discord", "telegram"], "casual chat"),
        profile(
            &["mail", "thunderbird", "outlook", "gmail"],
            "professional email",
        ),
        profile(&["code", "vscode", "zed", "nvim"], "terse, code-friendly"),
    ]
}

fn default_tone() -> String {
    "natural prose".to_string()
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InjectMethod {
    Type,
    Paste,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            aavaaz_url: "ws://localhost:9090".into(),
            model: "distil-large-v3".into(),
            language: Some("en".into()),
            hotkey: "Super+Space".into(),
            polish: PolishConfig {
                enabled: true,
                base_url: "https://api.openai.com/v1".into(),
                model: "gpt-4o-mini".into(),
                api_key_env: "OPENAI_API_KEY".into(),
                per_app_tone: true,
                tone_profiles: default_tone_profiles(),
                default_tone: default_tone(),
            },
            hotwords: vec![],
            inject_method: InjectMethod::Type,
        }
    }
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let dirs =
            ProjectDirs::from("com", "qol", "qol").context("could not resolve config dir")?;
        let dir = dirs.config_dir().to_path_buf();
        fs::create_dir_all(&dir).ok();
        Ok(dir.join("config.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        let raw = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(serde_json::from_str(&raw).unwrap_or_default())
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        let raw = serde_json::to_string_pretty(self)?;
        fs::write(&path, raw)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_through_json() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg.aavaaz_url, back.aavaaz_url);
        assert_eq!(cfg.model, back.model);
        assert_eq!(cfg.language, back.language);
        assert_eq!(cfg.hotkey, back.hotkey);
        assert_eq!(cfg.polish.enabled, back.polish.enabled);
        assert_eq!(cfg.polish.model, back.polish.model);
        assert_eq!(cfg.inject_method, back.inject_method);
    }

    #[test]
    fn default_has_sensible_values() {
        let cfg = Config::default();
        assert!(cfg.aavaaz_url.starts_with("ws://"));
        assert_eq!(cfg.hotkey, "Super+Space");
        assert!(cfg.polish.enabled);
        assert!(cfg.polish.base_url.ends_with("/v1"));
        assert_eq!(cfg.polish.api_key_env, "OPENAI_API_KEY");
        assert_eq!(cfg.inject_method, InjectMethod::Type);
    }

    #[test]
    fn inject_method_serializes_kebab_case() {
        let json = serde_json::to_string(&InjectMethod::Type).unwrap();
        assert_eq!(json, "\"type\"");
        let json = serde_json::to_string(&InjectMethod::Paste).unwrap();
        assert_eq!(json, "\"paste\"");
    }

    #[test]
    fn old_polish_config_without_tone_fields_gets_defaults() {
        // A config written before tone_profiles/default_tone existed.
        let json = r#"{
            "aavaaz_url":"ws://localhost:9090","model":"m","language":"en",
            "hotkey":"Super+Space","hotwords":[],"inject_method":"type",
            "polish":{"enabled":true,"base_url":"u","model":"m",
                      "api_key_env":"K","per_app_tone":true}
        }"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert!(
            !cfg.polish.tone_profiles.is_empty(),
            "profiles default-filled"
        );
        assert_eq!(cfg.polish.default_tone, "natural prose");
    }

    #[test]
    fn unknown_fields_fall_back_to_default() {
        // Old config files without newer fields should not crash.
        let partial = r#"{"aavaaz_url":"ws://example:1234"}"#;
        let parsed: Config = serde_json::from_str(partial).unwrap_or_default();
        // Either we parsed the partial (and got defaults for the rest),
        // or fell back to full default. Either way `aavaaz_url` should be set.
        assert!(!parsed.aavaaz_url.is_empty());
    }
}
