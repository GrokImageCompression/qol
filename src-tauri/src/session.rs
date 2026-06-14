use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::audio::CaptureHandle;
use crate::commands::{parse as parse_command, Command};
use crate::config::Config;
use crate::inject::{active_app_name, InjectorHandle};
use crate::polish::{polish, PolishContext};
use crate::transport::run_session;

pub struct Session {
    audio_tx: mpsc::UnboundedSender<Vec<f32>>,
    _capture: CaptureHandle,
    transport: JoinHandle<()>,
    collector: JoinHandle<()>,
}

impl Session {
    pub fn start(cfg: Config, injector: InjectorHandle) -> Result<Self> {
        let (audio_tx, audio_rx) = mpsc::unbounded_channel::<Vec<f32>>();
        let (seg_tx, mut seg_rx) = mpsc::unbounded_channel();

        let capture = CaptureHandle::start(audio_tx.clone())?;
        let app_context = active_app_name();

        let cfg_clone = cfg.clone();
        let transport = tokio::spawn(async move {
            if let Err(e) = run_session(cfg_clone, audio_rx, seg_tx).await {
                tracing::error!(error = ?e, "transport session ended");
            }
        });

        let polish_cfg = cfg.polish.clone();
        let inject_method = cfg.inject_method;
        let app_for_polish = app_context.clone();
        let injector_for_collector = injector.clone();
        // Rolling polished context — fresh per session so prior dictations
        // don't bleed into a new one's tone.
        let polish_ctx = PolishContext::new();

        let collector = tokio::spawn(async move {
            // Track how many chars the previous Text injection contributed
            // so "scratch that" can erase exactly that segment.
            let mut last_injected_len: usize = 0;
            let mut first_text = true;

            while let Some(seg) = seg_rx.recv().await {
                let raw = seg.text.trim().to_string();
                if raw.is_empty() {
                    continue;
                }
                match parse_command(&raw) {
                    Command::Text(text) => {
                        let polished = polish(
                            &polish_cfg,
                            &text,
                            app_for_polish.as_deref(),
                            &polish_ctx,
                        )
                        .await;
                        let to_inject = if first_text {
                            first_text = false;
                            polished
                        } else {
                            format!(" {polished}")
                        };
                        last_injected_len = to_inject.chars().count();
                        injector_for_collector.inject(to_inject, inject_method);
                    }
                    Command::Newline => {
                        injector_for_collector.newline();
                        last_injected_len = 1;
                        first_text = true;
                    }
                    Command::Paragraph => {
                        injector_for_collector.paragraph();
                        last_injected_len = 2;
                        first_text = true;
                    }
                    Command::ScratchLast => {
                        injector_for_collector.backspace(last_injected_len);
                        last_injected_len = 0;
                    }
                    Command::SelectAll => {
                        injector_for_collector.select_all();
                        last_injected_len = 0;
                    }
                }
            }
        });

        tracing::debug!(app = ?app_context, "session started");

        Ok(Self {
            audio_tx,
            _capture: capture,
            transport,
            collector,
        })
    }

    /// Stop capturing; the collector finalizes once the segment channel drains.
    pub async fn stop(self) {
        drop(self.audio_tx);
        // _capture drops here, stopping the cpal stream via its channel.
        let _ = self.transport.await;
        let _ = self.collector.await;
    }
}
