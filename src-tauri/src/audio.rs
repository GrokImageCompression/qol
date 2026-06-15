use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use rubato::{FastFixedIn, PolynomialDegree, Resampler};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex as StdMutex};
use std::thread;
use tokio::sync::mpsc;

pub const TARGET_SAMPLE_RATE: u32 = 16_000;
const RESAMPLER_CHUNK: usize = 1024;

/// Accumulates audio samples for ~1 second and logs the RMS so we can tell
/// whether the cpal callback is actually receiving non-silent audio. RMS
/// is reported on a 0–1 scale (1 = full scale). Anything under ~0.01 is
/// effectively silent; speech typically sits at 0.05–0.3.
struct RmsWindow {
    target_samples: usize,
    sum_sq: f64,
    count: usize,
}

impl RmsWindow {
    fn new(rate_per_second: usize) -> Self {
        Self {
            target_samples: rate_per_second.max(1),
            sum_sq: 0.0,
            count: 0,
        }
    }
    fn add(&mut self, mono: &[f32]) {
        for s in mono {
            self.sum_sq += (*s as f64) * (*s as f64);
            self.count += 1;
        }
        if self.count >= self.target_samples {
            let rms = (self.sum_sq / self.count as f64).sqrt();
            tracing::info!(
                rms = format!("{rms:.4}"),
                samples = self.count,
                "audio: 1s RMS"
            );
            self.sum_sq = 0.0;
            self.count = 0;
        }
    }
}

/// Send handle to a dedicated thread that owns the !Send `cpal::Stream`.
/// Drop the handle to stop capture.
pub struct CaptureHandle {
    _stop_tx: std_mpsc::Sender<()>,
}

impl CaptureHandle {
    pub fn start(samples_tx: mpsc::UnboundedSender<Vec<f32>>) -> Result<Self> {
        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let (ready_tx, ready_rx) = std_mpsc::channel::<Result<(), String>>();

        thread::Builder::new()
            .name("qol-audio".into())
            .spawn(move || {
                let stream = match build_stream(samples_tx) {
                    Ok(s) => {
                        let _ = ready_tx.send(Ok(()));
                        s
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(format!("{e}")));
                        return;
                    }
                };
                if let Err(e) = stream.play() {
                    tracing::error!(error = ?e, "stream play");
                    return;
                }
                let _ = stop_rx.recv();
                drop(stream);
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { _stop_tx: stop_tx }),
            Ok(Err(e)) => Err(anyhow!(e)),
            Err(e) => Err(anyhow!("audio thread died: {e}")),
        }
    }
}

/// Buffers mono f32 input from cpal callbacks and emits resampled 16 kHz chunks.
/// Wrapped in Arc<Mutex<...>> so multiple cpal sample-format closures can share it.
struct ResampleStage {
    resampler: Option<FastFixedIn<f32>>,
    buf: Vec<f32>,
    tx: mpsc::UnboundedSender<Vec<f32>>,
}

impl ResampleStage {
    fn new(src_rate: u32, tx: mpsc::UnboundedSender<Vec<f32>>) -> Result<Self> {
        let resampler = if src_rate == TARGET_SAMPLE_RATE {
            None
        } else {
            Some(
                FastFixedIn::new(
                    TARGET_SAMPLE_RATE as f64 / src_rate as f64,
                    1.0, // max_resample_ratio_relative
                    PolynomialDegree::Septic,
                    RESAMPLER_CHUNK,
                    1, // channels
                )
                .map_err(|e| anyhow!("rubato init: {e}"))?,
            )
        };
        Ok(Self {
            resampler,
            buf: Vec::with_capacity(RESAMPLER_CHUNK * 2),
            tx,
        })
    }

    fn push(&mut self, mono: &[f32]) {
        if self.resampler.is_none() {
            let _ = self.tx.send(mono.to_vec());
            return;
        }
        self.buf.extend_from_slice(mono);
        while self.buf.len() >= RESAMPLER_CHUNK {
            let chunk: Vec<f32> = self.buf.drain(..RESAMPLER_CHUNK).collect();
            let input: [&[f32]; 1] = [&chunk];
            let resampler = self.resampler.as_mut().expect("checked above");
            match resampler.process(&input, None) {
                Ok(out) => {
                    if let Some(channel0) = out.into_iter().next() {
                        let _ = self.tx.send(channel0);
                    }
                }
                Err(e) => {
                    tracing::error!(error = ?e, "rubato process");
                    return;
                }
            }
        }
    }
}

fn build_stream(samples_tx: mpsc::UnboundedSender<Vec<f32>>) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    tracing::info!(host = ?host.id(), "audio: cpal host");
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))?;
    let device_name = device.name().unwrap_or_else(|_| "<unknown>".into());
    let supported = device
        .default_input_config()
        .context("query default input config")?;
    let src_rate = supported.sample_rate().0;
    let channels = supported.channels() as usize;
    let format = supported.sample_format();
    tracing::info!(
        device = %device_name,
        rate = src_rate,
        channels,
        format = ?format,
        "audio: using input device",
    );
    let cfg: StreamConfig = supported.into();
    let err_fn = |e| tracing::error!(error = ?e, "audio input stream error");

    let stage = Arc::new(StdMutex::new(ResampleStage::new(src_rate, samples_tx)?));

    // Periodic-RMS counter for debugging "is the mic actually capturing?".
    // Logs roughly once per second of audio.
    let rms_window = Arc::new(StdMutex::new(RmsWindow::new(src_rate as usize)));
    let process = {
        let stage = stage.clone();
        let rms_window = rms_window.clone();
        move |samples: Vec<f32>| {
            let mono: Vec<f32> = if channels > 1 {
                samples
                    .chunks(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                    .collect()
            } else {
                samples
            };
            if let Ok(mut w) = rms_window.lock() {
                w.add(&mono);
            }
            if let Ok(mut g) = stage.lock() {
                g.push(&mono);
            }
        }
    };

    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            &cfg,
            {
                let process = process.clone();
                move |data: &[f32], _| process(data.to_vec())
            },
            err_fn,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            &cfg,
            {
                let process = process.clone();
                move |data: &[i16], _| {
                    process(data.iter().map(|s| *s as f32 / i16::MAX as f32).collect())
                }
            },
            err_fn,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            &cfg,
            {
                let process = process.clone();
                move |data: &[u16], _| {
                    process(
                        data.iter()
                            .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                            .collect(),
                    )
                }
            },
            err_fn,
            None,
        )?,
        other => return Err(anyhow!("unsupported sample format: {other:?}")),
    };
    Ok(stream)
}
