//! Audio output: a cpal stream on a dedicated thread, or a timer-driven null output for
//! machines without a sound card (CI, this container). The stream lives on its own
//! thread because `cpal::Stream` is not `Send` on every platform.

use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig, SupportedBufferSize};

use crate::engine::Renderer;
use crate::snapshot::Stats;

#[derive(Debug, Clone, Default)]
pub struct OutputOptions {
    /// Case-insensitive substring of the device name or id. `None` = default device.
    pub device: Option<String>,
    /// Requested buffer size in frames. `None` = backend default.
    pub buffer_frames: Option<u32>,
    /// Use the null output even if a device exists.
    pub null: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OutputInfo {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: Option<u32>,
    pub sample_format: String,
    pub null: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceSummary {
    pub id: String,
    pub name: String,
    pub max_channels: u16,
    pub is_default: bool,
}

pub fn list_devices() -> Vec<DeviceSummary> {
    let host = cpal::default_host();
    let default_id = host
        .default_output_device()
        .and_then(|d| d.id().ok())
        .map(|i| i.to_string());
    let Ok(devices) = host.output_devices() else {
        return vec![];
    };
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let name = d
                .description()
                .map(|x| x.name().to_string())
                .unwrap_or_else(|_| id.clone());
            let max_channels = d
                .supported_output_configs()
                .ok()?
                .map(|c| c.channels())
                .max()
                .unwrap_or(0);
            Some(DeviceSummary {
                is_default: Some(&id) == default_id.as_ref(),
                id,
                name,
                max_channels,
            })
        })
        .collect()
}

/// Keeps the output running until dropped.
pub struct OutputHandle {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Drop for OutputHandle {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Start output. `make_renderer` is called once the sample rate is known.
pub fn start(
    opts: &OutputOptions,
    stats: Arc<Stats>,
    make_renderer: impl FnOnce(u32) -> Renderer + Send + 'static,
) -> Result<(OutputHandle, OutputInfo), String> {
    if opts.null {
        return Ok(start_null(opts.buffer_frames.unwrap_or(256), make_renderer));
    }
    let (info_tx, info_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let opts2 = opts.clone();
    let thread = thread::Builder::new()
        .name("earshot-audio-owner".into())
        .spawn(move || {
            let built = build_stream(&opts2, stats, make_renderer);
            match built {
                Ok((stream, info)) => {
                    if let Err(e) = stream.play() {
                        let _ = info_tx.send(Err(e.to_string()));
                        return;
                    }
                    let _ = info_tx.send(Ok(info));
                    let _ = stop_rx.recv(); // park until the handle is dropped
                    drop(stream);
                }
                Err(e) => {
                    let _ = info_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let info = info_rx.recv().map_err(|e| e.to_string())??;
    Ok((
        OutputHandle {
            stop: Some(stop_tx),
            thread: Some(thread),
        },
        info,
    ))
}

fn build_stream(
    opts: &OutputOptions,
    stats: Arc<Stats>,
    make_renderer: impl FnOnce(u32) -> Renderer + Send + 'static,
) -> Result<(cpal::Stream, OutputInfo), String> {
    let host = cpal::default_host();
    let device = match &opts.device {
        Some(want) => {
            let want = want.to_lowercase();
            host.output_devices()
                .map_err(|e| e.to_string())?
                .find(|d| {
                    let id = d.id().map(|i| i.to_string()).unwrap_or_default();
                    let name = d
                        .description()
                        .map(|x| x.name().to_string())
                        .unwrap_or_default();
                    id.to_lowercase().contains(&want) || name.to_lowercase().contains(&want)
                })
                .ok_or_else(|| format!("no output device matching '{want}'"))?
        }
        None => host
            .default_output_device()
            .ok_or("no default output device")?,
    };
    let name = device
        .description()
        .map(|x| x.name().to_string())
        .unwrap_or_else(|_| "unknown".into());

    // Prefer 4+ channels (master + headphones), 44.1/48 kHz, float samples.
    let rank = |f: SampleFormat| match f {
        SampleFormat::F32 => 0,
        SampleFormat::I32 => 1,
        SampleFormat::I16 => 2,
        _ => 9,
    };
    let mut best: Option<(i32, cpal::SupportedStreamConfig)> = None;
    for range in device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
    {
        if rank(range.sample_format()) == 9 {
            continue;
        }
        let ch_score = if range.channels() >= 4 { 0 } else { 10 };
        for rate in [44_100u32, 48_000] {
            if let Some(cfg) = range.try_with_sample_rate(rate) {
                let score =
                    ch_score + rank(cfg.sample_format()) + if rate == 44_100 { 0 } else { 1 };
                if best.as_ref().is_none_or(|(s, _)| score < *s) {
                    best = Some((score, cfg));
                }
            }
        }
    }
    let supported = match best {
        Some((_, c)) => c,
        None => device.default_output_config().map_err(|e| e.to_string())?,
    };
    let mut buffer_frames = None;
    let mut config: StreamConfig = supported.config();
    if let Some(want) = opts.buffer_frames {
        let ok = match supported.buffer_size() {
            SupportedBufferSize::Range { min, max } => (*min..=*max).contains(&want),
            _ => true,
        };
        if ok {
            config.buffer_size = cpal::BufferSize::Fixed(want);
            buffer_frames = Some(want);
        }
    }
    let info = OutputInfo {
        device: name,
        sample_rate: config.sample_rate,
        channels: config.channels,
        buffer_frames,
        sample_format: format!("{:?}", supported.sample_format()),
        null: false,
    };
    let renderer = make_renderer(config.sample_rate);
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config, renderer, stats),
        SampleFormat::I32 => build::<i32>(&device, config, renderer, stats),
        SampleFormat::I16 => build::<i16>(&device, config, renderer, stats),
        f => return Err(format!("unsupported sample format {f:?}")),
    }?;
    Ok((stream, info))
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut renderer: Renderer,
    stats: Arc<Stats>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + 'static,
{
    let channels = config.channels as usize;
    // Conversion scratch, allocated up front; grows only if the backend hands us a
    // bigger buffer than it promised (which would be an allocation on the audio thread).
    let mut scratch = vec![0f32; 8192 * channels];
    let err_stats = stats.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                if scratch.len() < data.len() {
                    scratch.resize(data.len(), 0.0);
                }
                let buf = &mut scratch[..data.len()];
                renderer.process(buf, channels);
                for (o, s) in data.iter_mut().zip(buf.iter()) {
                    *o = T::from_sample(*s);
                }
            },
            move |err| {
                if err.kind() == cpal::ErrorKind::Xrun {
                    err_stats.xruns.fetch_add(1, Relaxed);
                } else {
                    eprintln!("audio stream error: {err}");
                }
            },
            None,
        )
        .map_err(|e| e.to_string())
}

fn start_null(
    frames: u32,
    make_renderer: impl FnOnce(u32) -> Renderer + Send + 'static,
) -> (OutputHandle, OutputInfo) {
    const RATE: u32 = 44_100;
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread = thread::Builder::new()
        .name("earshot-null-audio".into())
        .spawn(move || {
            let mut renderer = make_renderer(RATE);
            let mut buf = vec![0f32; frames as usize * 4];
            let period = Duration::from_secs_f64(frames as f64 / RATE as f64);
            let mut next = Instant::now();
            loop {
                match stop_rx.try_recv() {
                    Err(mpsc::TryRecvError::Empty) => {}
                    _ => break,
                }
                renderer.process(&mut buf, 4);
                next += period;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    thread::sleep(wait);
                }
            }
        })
        .expect("spawn null audio thread");
    let info = OutputInfo {
        device: "null (no audio device)".into(),
        sample_rate: RATE,
        channels: 4,
        buffer_frames: Some(frames),
        sample_format: "F32".into(),
        null: true,
    };
    (
        OutputHandle {
            stop: Some(stop_tx),
            thread: Some(thread),
        },
        info,
    )
}
