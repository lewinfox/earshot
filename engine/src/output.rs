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

use crate::engine::{PhonesRoute, Renderer};
use crate::phones_out::Aligner;
use crate::snapshot::Stats;

#[derive(Debug, Clone, Default)]
pub struct OutputOptions {
    /// Case-insensitive substring of the device name or id. `None` = default device.
    /// An id starting `alsa:` (`alsa:hw:CARD=DDJ400,DEV=0`) opens that ALSA device
    /// directly, bypassing the sound server: a controller's own sound card.
    pub device: Option<String>,
    /// Requested buffer size in frames. `None` = backend default.
    pub buffer_frames: Option<u32>,
    /// Use the null output even if a device exists.
    pub null: bool,
    /// The headphones on a device of their own (like [`OutputOptions::device`]).
    /// Only under PipeWire ([`can_split`]); `None`: on the master's device.
    pub phones_device: Option<String>,
    /// No output at all: audio is rendered only when [`crate::Engine::advance`] asks
    /// for it, so many frames at a time (tests that mustn't depend on timing).
    pub stepped: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OutputInfo {
    pub device: String,
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_frames: Option<u32>,
    pub sample_format: String,
    pub null: bool,
    /// The headphones' own device and its channels, if they have one.
    pub phones: Option<(String, u16)>,
    /// Why the headphones aren't on the device asked for (they're on the master's).
    pub phones_error: Option<String>,
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
    let pipewire = host.id().name().eq_ignore_ascii_case("pipewire");
    devices
        .filter_map(|d| {
            // PipeWire lists its "default" stand-ins and other programs' streams
            // too: only real devices have a driver.
            if pipewire && d.description().ok()?.driver().is_none() {
                return None;
            }
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

/// The audio chunk earshot asks PipeWire for, in frames at [`PIPEWIRE_RATE`]: about
/// 5 ms. It suits the default buffer (256 frames, two queued); a smaller buffer would
/// need a smaller chunk.
pub const PIPEWIRE_CHUNK: u32 = 256;
/// The rate PipeWire runs at unless set otherwise.
pub const PIPEWIRE_RATE: u32 = 48_000;

/// The headphones can go to a device of their own: PipeWire keeps the two devices'
/// clocks together (on Linux with PipeWire running, cpal talks to it directly).
pub fn can_split() -> bool {
    cfg!(target_os = "linux") && pipewire_running()
}

/// Whether PipeWire is the sound server: its socket is there.
pub fn pipewire_running() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    let dir =
        std::env::var_os("PIPEWIRE_RUNTIME_DIR").or_else(|| std::env::var_os("XDG_RUNTIME_DIR"));
    dir.is_some_and(|d| std::path::Path::new(&d).join("pipewire-0").exists())
}

/// Under PipeWire, ask it for small chunks of audio. cpal reaches PipeWire through its
/// stand-in ALSA card, which doesn't pass our buffer size on, so PipeWire works in its
/// default chunks (1024 frames, 21 ms): bigger than our queue, which runs dry and
/// stutters. PipeWire reads `PIPEWIRE_LATENCY` from each program it serves. Without
/// PipeWire (older Linux, raw ALSA, Mac, Windows) this does nothing. A value set by
/// the user stays.
///
/// # Safety
/// Sets an environment variable: call it first thing in `main`, before any other
/// thread starts (one reading the environment meanwhile is undefined behaviour).
pub unsafe fn ask_pipewire_for_small_chunks() {
    if std::env::var_os("PIPEWIRE_LATENCY").is_none() && pipewire_running() {
        // SAFETY: the caller promises no other thread is running.
        unsafe {
            std::env::set_var(
                "PIPEWIRE_LATENCY",
                format!("{PIPEWIRE_CHUNK}/{PIPEWIRE_RATE}"),
            )
        };
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
                Ok((streams, info)) => {
                    for s in &streams {
                        if let Err(e) = s.play() {
                            let _ = info_tx.send(Err(e.to_string()));
                            return;
                        }
                    }
                    let _ = info_tx.send(Ok(info));
                    let _ = stop_rx.recv(); // park until the handle is dropped
                    drop(streams);
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

#[cfg(target_os = "linux")]
fn alsa_host() -> Result<cpal::Host, String> {
    cpal::host_from_id(cpal::HostId::Alsa).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "linux"))]
fn alsa_host() -> Result<cpal::Host, String> {
    Err("ALSA devices are only on Linux".into())
}

/// The device asked for (by id, or part of its name), or the default.
fn find_device(host: &cpal::Host, want: Option<&str>) -> Result<cpal::Device, String> {
    match want {
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
                    id.to_lowercase() == want || name.to_lowercase().contains(&want)
                })
                .ok_or_else(|| format!("no output device matching '{want}'"))
        }
        None => host
            .default_output_device()
            .ok_or_else(|| "no default output device".into()),
    }
}

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|x| x.name().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

/// Whether `device` names an ALSA device to open directly ([`OutputOptions::device`]).
pub fn is_direct(device: Option<&str>) -> bool {
    device.is_some_and(|d| d.starts_with("alsa:"))
}

fn build_stream(
    opts: &OutputOptions,
    stats: Arc<Stats>,
    make_renderer: impl FnOnce(u32) -> Renderer + Send + 'static,
) -> Result<(Vec<cpal::Stream>, OutputInfo), String> {
    let direct = is_direct(opts.device.as_deref());
    let host = if direct {
        alsa_host()?
    } else {
        cpal::default_host()
    };
    let device = find_device(&host, opts.device.as_deref())?;
    let name = device_name(&device);

    // Prefer 4+ channels (master + headphones), 44.1/48 kHz, float samples. Of the
    // rates, the one the system runs at (PipeWire's 48 kHz, CoreAudio's or Windows'
    // current rate): any other is converted on the way, which costs time and smooth
    // timing.
    let native = if pipewire_running() && !direct {
        Some(PIPEWIRE_RATE)
    } else {
        device.default_output_config().ok().map(|c| c.sample_rate())
    };
    let rate_score = |rate: u32| match (Some(rate) == native, rate) {
        (true, _) => 0,
        (false, 44_100) => 1,
        _ => 2,
    };
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
                let score = ch_score + rank(cfg.sample_format()) + rate_score(rate);
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
    let mut info = OutputInfo {
        device: name,
        sample_rate: config.sample_rate,
        channels: config.channels,
        buffer_frames,
        sample_format: format!("{:?}", supported.sample_format()),
        null: false,
        phones: None,
        phones_error: None,
    };
    let mut renderer = make_renderer(config.sample_rate);
    // The headphones' device first: if it can't be had, they stay on the master's.
    let mut streams = vec![];
    if let Some(want) = &opts.phones_device {
        match phones_stream(&host, want, config.clone(), stats.clone()) {
            Ok((stream, (tx, route), (name, channels))) => {
                renderer.tap_phones(tx, route);
                streams.push(stream);
                info.phones = Some((name, channels));
            }
            Err(e) => {
                eprintln!("audio: headphones: {e}");
                info.phones_error = Some(e);
            }
        }
    }
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, config, renderer, stats),
        SampleFormat::I32 => build::<i32>(&device, config, renderer, stats),
        SampleFormat::I16 => build::<i16>(&device, config, renderer, stats),
        f => return Err(format!("unsupported sample format {f:?}")),
    }?;
    // The master's stream first, so it's the one that starts the engine.
    streams.insert(0, stream);
    Ok((streams, info))
}

/// What the headphones' stream hands back: the queue's end and their routing, for the
/// renderer.
type Tap = (rtrb::Producer<f32>, Arc<PhonesRoute>);

/// A stream on the headphones' own device, playing what the renderer queues for it
/// (at the master's rate and buffer size), kept in line by [`Aligner`].
fn phones_stream(
    host: &cpal::Host,
    want: &str,
    master: StreamConfig,
    stats: Arc<Stats>,
) -> Result<(cpal::Stream, Tap, (String, u16)), String> {
    if !can_split() {
        return Err("a separate headphones device needs PipeWire".into());
    }
    let device = find_device(host, Some(want))?;
    let name = device_name(&device);
    // Float at the master's rate; as many channels as it has (up to 4).
    let range = device
        .supported_output_configs()
        .map_err(|e| e.to_string())?
        .filter(|r| r.sample_format() == SampleFormat::F32)
        .filter_map(|r| r.try_with_sample_rate(master.sample_rate))
        .max_by_key(|c| c.channels().min(4))
        .ok_or_else(|| format!("{name} can't play at {} Hz", master.sample_rate))?;
    let mut config = range.config();
    config.buffer_size = master.buffer_size;
    let channels = config.channels;
    let chunk = match master.buffer_size {
        cpal::BufferSize::Fixed(n) => n as usize,
        cpal::BufferSize::Default => PIPEWIRE_CHUNK as usize,
    };
    // A chunk queued when the headphones go first in a cycle (the streams run in
    // either order): the least that never runs dry while the master keeps up.
    let (tx, rx) = rtrb::RingBuffer::new(master.sample_rate as usize * 2);
    let route = Arc::new(PhonesRoute::default());
    let mut aligner = Aligner::new(rx, chunk, route.clone(), stats.clone());
    let err_stats = stats;
    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [f32], _| {
                crate::dsp::flush_denormals();
                aligner.process(data, channels as usize)
            },
            move |err| {
                if err.kind() == cpal::ErrorKind::Xrun {
                    err_stats.xruns.fetch_add(1, Relaxed);
                } else {
                    eprintln!("audio stream error (headphones): {err}");
                }
            },
            None,
        )
        .map_err(|e| e.to_string())?;
    Ok((stream, (tx, route), (name, channels)))
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
                crate::dsp::flush_denormals();
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

/// The rate the null and stepped outputs run at.
pub(crate) const NULL_RATE: u32 = 44_100;

/// What a stepped engine says its output is ([`OutputOptions::stepped`]).
pub(crate) fn stepped_info(frames: u32) -> OutputInfo {
    OutputInfo {
        device: "stepped (no audio device)".into(),
        sample_rate: NULL_RATE,
        channels: 4,
        buffer_frames: Some(frames),
        sample_format: "F32".into(),
        null: true,
        phones: None,
        phones_error: None,
    }
}

fn start_null(
    frames: u32,
    make_renderer: impl FnOnce(u32) -> Renderer + Send + 'static,
) -> (OutputHandle, OutputInfo) {
    const RATE: u32 = NULL_RATE;
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
        phones: None,
        phones_error: None,
    };
    (
        OutputHandle {
            stop: Some(stop_tx),
            thread: Some(thread),
        },
        info,
    )
}
