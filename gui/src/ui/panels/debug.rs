//! Debug mode's panels (debug builds only): the app's log, a spectrogram of what's
//! going to the sound card, and a button that exports the last so many seconds of it
//! all to a zip file (`core::debug`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use egui::{Color32, ColorImage, RichText, TextureHandle, TextureOptions, Ui, pos2};
use realfft::{RealFftPlanner, RealToComplex};

use super::Cx;
use crate::core::debug::{MAX_SECS, Recorder};

/// The spectrogram's FFT length, the step between its columns (frames), how many
/// columns it shows and its rows (log frequency, 20 Hz to Nyquist).
const FFT: usize = 2048;
const HOP: usize = 1024;
const COLUMNS: usize = 512;
const ROWS: usize = 256;
/// Levels shown, dB below a full-scale sine.
const FLOOR_DB: f32 = -100.0;
/// Log lines shown.
const LOG_LINES: usize = 500;

pub struct DebugUi {
    /// The recorder (debug builds); `None` and the mode shows nothing.
    pub recorder: Option<Recorder>,
    /// What the app knows that a report should carry (audio device, settings).
    pub info: String,
    /// The controller lights' slot, to resend the controllers' start messages.
    pub lights: Option<crate::core::lights::MonitorSlot>,
    /// The controller mapping, to name what each message does.
    pub mapping: Option<crate::core::midi::Learned>,
    /// Seconds an export holds.
    secs: u32,
    /// Which channel pair the spectrogram shows (0: channels 1-2).
    pair: usize,
    spectrogram: Spectrogram,
    log: Vec<String>,
    log_seen: u64,
    /// The last export's path, or why it failed; `None` while it runs.
    exported: Arc<Mutex<Option<String>>>,
    exporting: bool,
}

impl Default for DebugUi {
    fn default() -> DebugUi {
        DebugUi {
            recorder: None,
            info: String::new(),
            lights: None,
            mapping: None,
            secs: 30,
            pair: 0,
            spectrogram: Spectrogram::new(),
            log: vec![],
            log_seen: u64::MAX,
            exported: Arc::default(),
            exporting: false,
        }
    }
}

impl DebugUi {
    /// The bar: export, how many seconds, which channels.
    pub fn bar(&mut self, ui: &mut Ui, cx: &Cx) {
        let Some(rec) = self.recorder.clone() else {
            ui.label("Debug mode needs a debug build.");
            return;
        };
        ui.horizontal(|ui| {
            let busy = self.exporting && self.exported.lock().unwrap().is_none();
            let b = ui
                .add_enabled(!busy, egui::Button::new("Export data"))
                .on_hover_text(
                    "Write the last seconds of the sound sent to the sound card, the engine's \
                     state, this run's log and the settings to a zip file",
                );
            ui.label("last");
            ui.add(
                egui::DragValue::new(&mut self.secs)
                    .range(1..=MAX_SECS)
                    .suffix(" s"),
            );
            if b.clicked() {
                self.exporting = true;
                *self.exported.lock().unwrap() = None;
                let (rec, secs, info, done, mapping) = (
                    rec.clone(),
                    self.secs,
                    self.info.clone(),
                    self.exported.clone(),
                    self.mapping.clone(),
                );
                let data = crate::core::folders::state_folder();
                std::thread::spawn(move || {
                    // The controller mapping as it is now.
                    let extra = mapping
                        .as_ref()
                        .map(|m| {
                            vec![(
                                "controller-mapping.json".to_string(),
                                m.to_json("session").into_bytes(),
                            )]
                        })
                        .unwrap_or_default();
                    let dir = data.join("debug");
                    let r = match rec.export(secs, &dir, &info, mapping.as_ref(), extra) {
                        Ok(p) => format!("Exported to {}", p.display()),
                        Err(e) => format!("Export failed: {e}"),
                    };
                    *done.lock().unwrap() = Some(r);
                });
            }
            if busy {
                ui.spinner();
            } else if self.exporting
                && let Some(note) = &*self.exported.lock().unwrap()
            {
                ui.add(
                    egui::Label::new(RichText::new(note).color(cx.theme.muted)).selectable(true),
                );
            }
            ui.separator();
            if ui
                .add_enabled(
                    self.lights.is_some(),
                    egui::Button::new("Resend start messages"),
                )
                .on_hover_text(
                    "Send the controller the start messages from its controller file again \
                     (a DDJ-400's asks where its controls are). It's logged and in the \
                     export's MIDI.",
                )
                .clicked()
                && let Some(l) = &self.lights
            {
                l.lock().unwrap().resend_start = true;
            }
            ui.separator();
            let channels = rec.history.lock().unwrap().channels;
            let pairs = channels.div_ceil(2).max(1);
            egui::ComboBox::from_id_salt("debug-pair")
                .selected_text(pair_name(self.pair))
                .show_ui(ui, |ui| {
                    for p in 0..pairs {
                        ui.selectable_value(&mut self.pair, p, pair_name(p));
                    }
                });
            let dropped = rec.history.lock().unwrap().dropped;
            if dropped > 0 {
                ui.label(
                    RichText::new(format!("{dropped} block(s) not recorded")).color(cx.theme.warn),
                );
            }
        });
        if busy_repaint(self) {
            ui.ctx().request_repaint();
        }
    }

    /// The app's log, newest at the bottom.
    pub fn log(&mut self, ui: &mut Ui) {
        if crate::core::log::written() != self.log_seen {
            let (lines, seen) = crate::core::log::recent();
            let skip = lines.len().saturating_sub(LOG_LINES);
            self.log = lines.into_iter().skip(skip).collect();
            self.log_seen = seen;
        }
        egui::ScrollArea::both()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for line in &self.log {
                    ui.add(egui::Label::new(RichText::new(line).monospace().size(11.0)).extend());
                }
            });
    }

    /// The spectrogram of the chosen channels, newest on the right.
    pub fn spectrogram(&mut self, ui: &mut Ui, cx: &Cx) {
        let Some(rec) = &self.recorder else { return };
        let rate = {
            let h = rec.history.lock().unwrap();
            self.spectrogram.feed(&h, self.pair);
            h.rate
        };
        let size = ui.available_size();
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        let tex = self.spectrogram.texture(ui.ctx());
        ui.painter().image(
            tex.id(),
            rect,
            egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        // Frequency marks.
        if rate > 0 {
            let nyquist = rate as f32 / 2.0;
            for (f, name) in [(100.0, "100"), (1000.0, "1k"), (10_000.0, "10k")] {
                let y = rect.bottom() - rect.height() * freq_row(f, nyquist);
                ui.painter().text(
                    pos2(rect.left() + 4.0, y),
                    egui::Align2::LEFT_CENTER,
                    name,
                    egui::FontId::monospace(10.0),
                    cx.theme.text,
                );
            }
        }
        ui.ctx().request_repaint();
    }
}

fn busy_repaint(d: &DebugUi) -> bool {
    d.exporting && d.exported.lock().unwrap().is_none()
}

fn pair_name(p: usize) -> String {
    format!("Channels {}-{}", 2 * p + 1, 2 * p + 2)
}

/// Where frequency `f` sits, 0 (20 Hz) to 1 (Nyquist), on a log scale.
fn freq_row(f: f32, nyquist: f32) -> f32 {
    ((f / 20.0).ln() / (nyquist / 20.0).ln()).clamp(0.0, 1.0)
}

struct Spectrogram {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    /// The next frame to read, and samples read but not yet a column.
    next: Option<u64>,
    pending: Vec<f32>,
    pair: usize,
    columns: VecDeque<[Color32; ROWS]>,
    texture: Option<TextureHandle>,
    rate: u32,
}

impl Spectrogram {
    fn new() -> Spectrogram {
        let window = (0..FFT)
            .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / FFT as f32).cos())
            .collect();
        Spectrogram {
            fft: RealFftPlanner::<f32>::new().plan_fft_forward(FFT),
            window,
            next: None,
            pending: vec![],
            pair: 0,
            columns: VecDeque::new(),
            texture: None,
            rate: 0,
        }
    }

    /// Turn what's new in `h` into columns.
    fn feed(&mut self, h: &crate::core::debug::History, pair: usize) {
        if pair != self.pair || h.rate != self.rate {
            self.pair = pair;
            self.rate = h.rate;
            self.next = None;
            self.columns.clear();
        }
        // First time (or after a change): only the last screenful.
        let recent = h.frames.saturating_sub((COLUMNS * HOP + FFT) as u64);
        let from = self.next.unwrap_or(recent).max(recent);
        if self.next != Some(from) {
            self.pending.clear();
        }
        let (start, mono) = h.mono_since(from, pair);
        if start != from {
            self.pending.clear();
        }
        self.next = Some(start + mono.len() as u64);
        self.pending.extend(mono);
        let mut input = self.fft.make_input_vec();
        let mut output = self.fft.make_output_vec();
        let mut taken = 0;
        while self.pending.len() - taken >= FFT {
            for (i, x) in input.iter_mut().enumerate() {
                *x = self.pending[taken + i] * self.window[i];
            }
            let _ = self.fft.process(&mut input, &mut output);
            self.columns.push_back(column(&output, self.rate));
            if self.columns.len() > COLUMNS {
                self.columns.pop_front();
            }
            taken += HOP;
        }
        self.pending.drain(..taken);
    }

    fn texture(&mut self, ctx: &egui::Context) -> &TextureHandle {
        let mut img = ColorImage::filled([COLUMNS, ROWS], Color32::BLACK);
        let offset = COLUMNS - self.columns.len();
        for (x, col) in self.columns.iter().enumerate() {
            for (row, c) in col.iter().enumerate() {
                img[(offset + x, ROWS - 1 - row)] = *c;
            }
        }
        match &mut self.texture {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("debug-spectrogram", img, TextureOptions::LINEAR))
            }
        }
        self.texture.as_ref().expect("just set")
    }
}

/// One column: each row's loudest bin, coloured by level.
fn column(bins: &[realfft::num_complex::Complex<f32>], rate: u32) -> [Color32; ROWS] {
    let nyquist = rate.max(1) as f32 / 2.0;
    let hz_per_bin = nyquist / (bins.len() - 1) as f32;
    // A full-scale sine through the Hann window peaks at FFT / 4.
    let full = FFT as f32 / 4.0;
    let mut out = [Color32::BLACK; ROWS];
    for (row, c) in out.iter_mut().enumerate() {
        let f0 = 20.0 * (nyquist / 20.0).powf(row as f32 / ROWS as f32);
        let f1 = 20.0 * (nyquist / 20.0).powf((row + 1) as f32 / ROWS as f32);
        let b0 = (f0 / hz_per_bin) as usize;
        let b1 = ((f1 / hz_per_bin) as usize).max(b0 + 1).min(bins.len());
        let mag = bins[b0.min(bins.len() - 1)..b1]
            .iter()
            .map(|b| b.norm())
            .fold(0.0, f32::max);
        let db = 20.0 * (mag / full).max(1e-9).log10();
        *c = heat(((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0));
    }
    out
}

/// Black, blue, magenta, orange, yellow, white for 0 to 1.
fn heat(t: f32) -> Color32 {
    const STOPS: [[f32; 3]; 6] = [
        [0.0, 0.0, 0.0],
        [0.1, 0.1, 0.6],
        [0.6, 0.1, 0.6],
        [0.95, 0.4, 0.1],
        [1.0, 0.85, 0.2],
        [1.0, 1.0, 1.0],
    ];
    let x = t * (STOPS.len() - 1) as f32;
    let i = (x as usize).min(STOPS.len() - 2);
    let k = x - i as f32;
    let [a, b] = [STOPS[i], STOPS[i + 1]];
    let ch = |n: usize| ((a[n] + (b[n] - a[n]) * k) * 255.0) as u8;
    Color32::from_rgb(ch(0), ch(1), ch(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 kHz tone lights the row around 1 kHz and leaves 100 Hz and 10 kHz dark.
    #[test]
    fn a_tone_shows_at_its_frequency() {
        let rate = 48_000;
        let mut s = Spectrogram::new();
        s.rate = rate;
        let tone: Vec<f32> = (0..FFT)
            .map(|i| (std::f32::consts::TAU * 1000.0 * i as f32 / rate as f32).sin())
            .collect();
        let mut input = s.fft.make_input_vec();
        let mut output = s.fft.make_output_vec();
        for (i, x) in input.iter_mut().enumerate() {
            *x = tone[i] * s.window[i];
        }
        s.fft.process(&mut input, &mut output).unwrap();
        let col = column(&output, rate);
        let row = |f: f32| ((freq_row(f, rate as f32 / 2.0) * ROWS as f32) as usize).min(ROWS - 1);
        let level = |c: Color32| c.r() as u32 + c.g() as u32 + c.b() as u32;
        assert!(
            level(col[row(1000.0)]) > 600,
            "bright at 1 kHz: {:?}",
            col[row(1000.0)]
        );
        assert!(level(col[row(100.0)]) < level(col[row(1000.0)]) / 2);
        assert!(level(col[row(10_000.0)]) < level(col[row(1000.0)]) / 2);
    }
}
