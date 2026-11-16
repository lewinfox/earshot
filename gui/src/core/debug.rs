//! Debug builds only: a recorder for chasing problems you can hear. A thread keeps
//! the last [`MAX_SECS`] of everything the engine sends to the sound card (every
//! channel, as the device gets it) and the engine's state ten times a second.
//! [`Recorder::export`] writes the last so many seconds of it, with this run's log
//! and whatever else the app adds, to a zip file to hand over.
//!
//! The app gives it the engine's output tap and monitor ([`Feed`]) each time audio
//! starts. Debug mode (`ui::views::debug`) shows the log and a spectrogram from it.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use earshot_engine::{Monitor, OutputTap};

/// How much is kept, and so the most an export can hold.
pub const MAX_SECS: u32 = 120;
/// How often the thread empties the tap (it holds a few seconds).
const TICK: Duration = Duration::from_millis(20);
/// How often the engine's state is noted.
const STATE_EVERY: Duration = Duration::from_millis(100);

/// What the recorder reads, put here by the app when audio (re)starts.
#[derive(Default)]
pub struct Feed {
    pub tap: Option<OutputTap>,
    pub monitor: Option<Monitor>,
}

/// What's been recorded.
#[derive(Default)]
pub struct History {
    pub rate: u32,
    pub channels: usize,
    /// Interleaved, `channels` to a frame, oldest first.
    audio: VecDeque<f32>,
    /// Frames received since the recorder started; the newest frame is `frames - 1`.
    pub frames: u64,
    /// The engine's state, as `Debug` text, oldest first.
    states: VecDeque<(Instant, String)>,
    /// Blocks the engine couldn't copy because the tap was full.
    pub dropped: u64,
    /// Underruns and overruns the sound card has reported (this engine's).
    pub xruns: u64,
    /// MIDI both ways, oldest first: when, whether earshot sent it (else the
    /// controller did), and the message.
    midi: VecDeque<(Instant, bool, Vec<u8>)>,
}

impl History {
    /// The oldest frame still kept.
    fn first_frame(&self) -> u64 {
        let kept = self.audio.len() / self.channels.max(1);
        self.frames - kept as u64
    }

    /// Channel pair `pair` (0: channels 1-2) mixed to mono, from frame `from` (or the
    /// oldest kept, if that's gone) to the newest. Returns the first frame's number.
    pub fn mono_since(&self, from: u64, pair: usize) -> (u64, Vec<f32>) {
        let ch = self.channels;
        if ch == 0 {
            return (self.frames, vec![]);
        }
        let start = from.max(self.first_frame());
        let skip = (start - self.first_frame()) as usize;
        let (l, r) = (2 * pair, (2 * pair + 1).min(ch - 1));
        if l >= ch {
            return (start, vec![0.0; (self.frames - start) as usize]);
        }
        let frames = (self.frames - start) as usize;
        let out = (0..frames)
            .map(|i| {
                let at = (skip + i) * ch;
                (self.audio[at + l] + self.audio[at + r]) * 0.5
            })
            .collect();
        (start, out)
    }
}

/// The recorder: its history, and where it reads from. Cloning shares both.
#[derive(Clone, Default)]
pub struct Recorder {
    pub history: Arc<Mutex<History>>,
    pub feed: Arc<Mutex<Feed>>,
}

impl Recorder {
    /// Note a MIDI message: `out` if earshot sent it to the controller.
    pub fn note_midi(&self, out: bool, bytes: &[u8]) {
        let now = Instant::now();
        let mut h = self.history.lock().unwrap();
        h.midi.push_back((now, out, bytes.to_vec()));
        let keep = Duration::from_secs(MAX_SECS as u64);
        while h.midi.front().is_some_and(|(t, _, _)| now - *t > keep) {
            h.midi.pop_front();
        }
    }

    /// Start recording, on a thread of its own that runs until the app quits.
    pub fn start() -> Recorder {
        let r = Recorder::default();
        let (history, feed) = (r.history.clone(), r.feed.clone());
        let _ = std::thread::Builder::new()
            .name("earshot-recorder".into())
            .spawn(move || run(&history, &feed));
        r
    }

    /// Write the last `secs` seconds (as much as there is) to a zip file in `dir`:
    /// `output.wav` (every device channel, 32-bit float), `state.txt` (with the xrun
    /// count), `log.txt` (this run's whole log), `info.txt` (`info`, and what was
    /// recorded), `asound.txt` (the sound cards as the system has them open) and
    /// `extra` files.
    /// Returns its path.
    pub fn export(
        &self,
        secs: u32,
        dir: &Path,
        info: &str,
        mapping: Option<&super::midi::Learned>,
        extra: Vec<(String, Vec<u8>)>,
    ) -> Result<PathBuf, String> {
        let secs = secs.clamp(1, MAX_SECS);
        // Copied out under the lock; written after.
        let (wav, states, summary, controller) = {
            let h = self.history.lock().unwrap();
            let ch = h.channels.max(1);
            let want = secs as usize * h.rate as usize * ch;
            let skip = h.audio.len().saturating_sub(want);
            let samples: Vec<f32> = h.audio.iter().skip(skip).copied().collect();
            let since = Instant::now() - Duration::from_secs(secs as u64);
            let now = Instant::now();
            let states: String = h
                .states
                .iter()
                .filter(|(t, _)| *t >= since)
                .map(|(t, s)| format!("-{:.2}s {s}\n", (now - *t).as_secs_f64()))
                .collect();
            let summary = format!(
                "recorded: {:.1} s of {} channel(s) at {} Hz; {} block(s) dropped (the tap was \
                 full); {} xrun(s) reported by the sound card\n",
                samples.len() as f64 / ch as f64 / h.rate.max(1) as f64,
                h.channels,
                h.rate,
                h.dropped,
                h.xruns
            );
            let controller = controller_text(&h.midi, since, now, mapping);
            (wav_f32(&samples, h.channels.max(1) as u16, h.rate), states, summary, controller)
        };
        let log = super::log::path()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_else(|| b"(no log file)\n".to_vec());
        let mut files = vec![
            ("info.txt".to_string(), format!("{info}\n{summary}").into_bytes()),
            ("output.wav".to_string(), wav),
            ("state.txt".to_string(), states.into_bytes()),
            ("log.txt".to_string(), log),
            ("asound.txt".to_string(), asound().into_bytes()),
            ("controller.txt".to_string(), controller.into_bytes()),
        ];
        files.extend(extra);
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let path = dir.join(format!("earshot-debug-{unix}.zip"));
        std::fs::write(&path, zip_stored(&files)).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}

/// The controller's state and MIDI: the newest value of every message it has sent
/// (all of them, not only the last seconds), with the command it's mapped to; then
/// the MIDI both ways since `since`.
fn controller_text(
    midi: &VecDeque<(Instant, bool, Vec<u8>)>,
    since: Instant,
    now: Instant,
    mapping: Option<&super::midi::Learned>,
) -> String {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02X}")).collect::<Vec<_>>().join(" ");
    let mut latest: std::collections::BTreeMap<(u8, u8), (u8, Instant)> = Default::default();
    for (t, out, b) in midi {
        if !*out && b.len() == 3 && b[0] < 0xF0 {
            // A note off is a 0.
            let v = if b[0] & 0xF0 == 0x80 { 0 } else { b[2] };
            let status = if b[0] & 0xF0 == 0x80 { b[0] | 0x10 } else { b[0] };
            latest.insert((status, b[1]), (v, *t));
        }
    }
    let mut out = String::from(
        "Controls: the newest value the controller sent for each message (a DDJ-400 is asked \
         to report all of them just before an export), and what it's mapped to.\n\n",
    );
    for ((s, d), (v, t)) in &latest {
        let mapped = mapping.and_then(|m| m.get((*s, *d))).map_or("-".to_string(), |l| {
            let name = l.control.name().0;
            if l.display_only { format!("{name} (display only)") } else { name }
        });
        out += &format!(
            "{s:02X} {d:02X} = {v:3}  {:>8.2}s ago  {mapped}\n",
            (now - *t).as_secs_f64()
        );
    }
    out += "\nMIDI since the export's start (in: from the controller, out: from earshot):\n\n";
    for (t, sent, b) in midi.iter().filter(|(t, _, _)| *t >= since) {
        out += &format!(
            "-{:.3}s {} {}\n",
            (now - *t).as_secs_f64(),
            if *sent { "out" } else { "in " },
            hex(b)
        );
    }
    out
}

/// The sound cards as the system sees them now: each card, and for every playback
/// stream how it's open (`hw_params`: format, rate, buffer), its live `status`, and
/// for USB cards what they offer (`stream0`).
fn asound() -> String {
    let mut out = String::new();
    let mut add = |path: &Path| {
        if let Ok(text) = std::fs::read_to_string(path) {
            out += &format!("==== {}\n{text}\n", path.display());
        }
    };
    let root = Path::new("/proc/asound");
    add(&root.join("cards"));
    let Ok(cards) = std::fs::read_dir(root) else {
        return out;
    };
    let mut cards: Vec<PathBuf> = cards
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("card")))
        .collect();
    cards.sort();
    for card in cards {
        add(&card.join("id"));
        add(&card.join("stream0"));
        let Ok(pcms) = std::fs::read_dir(&card) else { continue };
        let mut pcms: Vec<PathBuf> = pcms
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("pcm") && n.ends_with('p')))
            .collect();
        pcms.sort();
        for pcm in pcms {
            for f in ["sub0/hw_params", "sub0/sw_params", "sub0/status"] {
                add(&pcm.join(f));
            }
        }
    }
    out
}

fn run(history: &Mutex<History>, feed: &Mutex<Feed>) {
    let mut last_state = Instant::now();
    loop {
        {
            let mut feed = feed.lock().unwrap();
            if let Some(tap) = &mut feed.tap {
                drain(tap, &mut history.lock().unwrap());
            }
            if last_state.elapsed() >= STATE_EVERY {
                last_state = Instant::now();
                if let Some(m) = &feed.monitor {
                    let mut h = history.lock().unwrap();
                    let line = format!("xruns={} {:?}", h.xruns, m.snapshot());
                    h.states.push_back((last_state, line));
                    let keep = Duration::from_secs(MAX_SECS as u64);
                    while h.states.front().is_some_and(|(t, _)| last_state - *t > keep) {
                        h.states.pop_front();
                    }
                }
            }
        }
        std::thread::sleep(TICK);
    }
}

/// Move what the tap has into the history, dropping the oldest past [`MAX_SECS`].
fn drain(tap: &mut OutputTap, h: &mut History) {
    let ch = tap.channels();
    if ch == 0 {
        return;
    }
    if h.rate != tap.sample_rate || h.channels != ch {
        // Another device: start again.
        h.audio.clear();
        h.rate = tap.sample_rate;
        h.channels = ch;
    }
    let n = tap.samples.slots();
    let n = n - n % ch;
    if n > 0
        && let Ok(chunk) = tap.samples.read_chunk(n)
    {
        let (a, b) = chunk.as_slices();
        h.audio.extend(a);
        h.audio.extend(b);
        chunk.commit_all();
        h.frames += (n / ch) as u64;
    }
    let cap = MAX_SECS as usize * h.rate as usize * ch;
    let over = h.audio.len().saturating_sub(cap);
    h.audio.drain(..over);
    h.dropped = tap.dropped();
    h.xruns = tap.xruns();
}

/// A WAV file of 32-bit float samples, `channels` interleaved.
fn wav_f32(samples: &[f32], channels: u16, rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 4) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend(b"RIFF");
    w.extend((36 + data_len).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(3u16.to_le_bytes()); // IEEE float
    w.extend(channels.to_le_bytes());
    w.extend(rate.to_le_bytes());
    w.extend((rate * channels as u32 * 4).to_le_bytes());
    w.extend((channels * 4).to_le_bytes());
    w.extend(32u16.to_le_bytes());
    w.extend(b"data");
    w.extend(data_len.to_le_bytes());
    for s in samples {
        w.extend(s.to_le_bytes());
    }
    w
}

/// A zip file holding `files` as they are (stored, not compressed).
fn zip_stored(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![];
    let mut central = vec![];
    for (name, data) in files {
        let crc = crc32fast::hash(data);
        let offset = out.len() as u32;
        let (size, name_len) = (data.len() as u32, name.len() as u16);
        // Local header: version 2.0, no flags, stored, no date.
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend(20u16.to_le_bytes());
        out.extend([0u8; 8]); // flags, method, time, date
        out.extend(crc.to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend(name_len.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(name.as_bytes());
        out.extend(data);
        // Its central directory entry.
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend(20u16.to_le_bytes());
        central.extend([0u8; 8]);
        central.extend(crc.to_le_bytes());
        central.extend(size.to_le_bytes());
        central.extend(size.to_le_bytes());
        central.extend(name_len.to_le_bytes());
        central.extend([0u8; 12]); // extra, comment, disk, internal attrs, external attrs
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let (at, len, n) = (out.len() as u32, central.len() as u32, files.len() as u16);
    out.extend(central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]); // disk numbers
    out.extend(n.to_le_bytes());
    out.extend(n.to_le_bytes());
    out.extend(len.to_le_bytes());
    out.extend(at.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The newest value of each control, a note off read as 0, and the log both ways.
    #[test]
    fn controller_text_has_each_controls_last_value_and_the_log() {
        let now = Instant::now();
        let t = |ms: u64| now - Duration::from_millis(ms);
        let midi: VecDeque<_> = [
            (t(3000), false, vec![0xB6, 0x0C, 10]),
            (t(2000), false, vec![0xB6, 0x0C, 64]),
            (t(1500), false, vec![0x90, 0x0B, 0x7F]),
            (t(1000), false, vec![0x80, 0x0B, 0x40]),
            (t(500), true, vec![0xB0, 0x02, 87]),
        ]
        .into();
        let text = controller_text(&midi, t(2500), now, None);
        assert!(text.contains("B6 0C =  64"), "{text}");
        assert!(text.contains("90 0B =   0"), "a note off is a 0: {text}");
        assert!(!text.contains("B0 02 ="), "earshot's own messages aren't controls");
        assert!(text.contains("out B0 02 57"), "{text}");
        assert!(!text.contains("in  B6 0C 0A"), "before the window: {text}");
    }

    /// On Linux there's at least the list of cards.
    #[test]
    fn asound_lists_the_cards() {
        if Path::new("/proc/asound/cards").exists() {
            assert!(asound().contains("==== /proc/asound/cards"));
        }
    }

    #[test]
    fn mono_since_mixes_a_pair_from_a_frame() {
        let mut h = History {
            rate: 10,
            channels: 4,
            ..History::default()
        };
        // Three frames: master (1, 3), phones (100, 100).
        for i in 0..3 {
            h.audio.extend([1.0 + i as f32, 3.0 + i as f32, 100.0, 100.0]);
        }
        h.frames = 3;
        assert_eq!(h.mono_since(0, 0), (0, vec![2.0, 3.0, 4.0]));
        assert_eq!(h.mono_since(2, 0), (2, vec![4.0]));
        assert_eq!(h.mono_since(1, 1), (1, vec![100.0, 100.0]));
        assert_eq!(h.mono_since(0, 5).1.len(), 3, "no such pair: silence");
    }

    /// The zip opens with `unzip` and has the files as they were.
    #[test]
    fn a_zip_unzip_can_read() {
        let files = vec![
            ("a.txt".to_string(), b"hello\n".to_vec()),
            ("b.wav".to_string(), wav_f32(&[0.0, 0.5, -0.5, 1.0], 2, 48_000)),
        ];
        let dir = std::env::temp_dir().join(format!("earshot-zip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.zip");
        std::fs::write(&path, zip_stored(&files)).unwrap();
        let Ok(out) = std::process::Command::new("unzip").arg("-p").arg(&path).arg("a.txt").output() else {
            return; // no unzip here
        };
        assert_eq!(out.stdout, b"hello\n", "{}", String::from_utf8_lossy(&out.stderr));
        let test = std::process::Command::new("unzip").arg("-t").arg(&path).output().unwrap();
        assert!(test.status.success(), "{}", String::from_utf8_lossy(&test.stdout));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
