//! The sampler: 16 slots, each a sound file with a volume and a loop switch, as
//! rekordbox has them on the DDJ-400's SAMPLER pad mode (slots 1-8 on the left deck's
//! pads, 9-16 on the right's; a pad plays its slot, SHIFT + pad stops it).
//!
//! - The [`Bank`] is what's saved: each slot's file and settings, as one setting in
//!   the Store (`settings::SAMPLER`).
//! - A slot's sound is decoded whole into memory on a background thread
//!   ([`load`], as tracks load) and handed to the engine, which plays it
//!   (`earshot_engine::sampler`). The player keeps each sound too ([`Session`]), to
//!   give a new engine after an audio restart.
//! - Playing and stopping go straight to the engine ([`SamplerCmd::direct`]), so a
//!   pad on the controller needs no lock; the rest change the bank, and the UI runs
//!   them.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use earshot_engine::decode::{self, TrackAudio};
use earshot_engine::media::Location;
use earshot_engine::{
    Command as EngineCommand, SAMPLER_SLOTS, SampleSlot, SampleSnap, SampleState,
    SamplerCmd as EngineSamplerCmd, Set,
};
use serde::{Deserialize, Serialize};

use super::lights::Light;
use super::player::Player;

/// How many slots there are.
pub const SLOTS: usize = SAMPLER_SLOTS;
/// Slots to a deck's pads.
pub const PADS: usize = 8;

/// The most of a file a slot holds, in seconds: the sampler is for short sounds, kept
/// whole in memory (about 20 MB a minute). A longer file is cut off there.
pub const MAX_SECS: f64 = 120.0;

/// The slot behind pad `pad` (0-7) of deck `deck` (0-based): slots 1-8 on the left
/// decks (1 and 3), 9-16 on the right (2 and 4).
pub fn pad_slot(deck: usize, pad: usize) -> Option<SampleSlot> {
    (pad < PADS).then(|| SampleSlot::new((deck % 2) * PADS + pad))?
}

/// One slot, as saved.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Slot {
    /// The sound's file (`None`: the slot is empty).
    pub file: Option<PathBuf>,
    /// 0..1, a fader.
    pub volume: f64,
    /// Play round and round, not once through.
    pub looping: bool,
}

impl Default for Slot {
    fn default() -> Slot {
        Slot {
            file: None,
            volume: 1.0,
            looping: false,
        }
    }
}

impl Slot {
    /// What to call the sound: its file's name, without the extension.
    pub fn name(&self) -> Option<String> {
        let file = self.file.as_ref()?;
        let name = file.file_stem().or(file.file_name())?;
        Some(name.to_string_lossy().into_owned())
    }
}

/// Every slot, as saved (a list in the Store; fewer or more than [`SLOTS`] read as
/// the first [`SLOTS`], the rest empty).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(from = "Vec<Slot>", into = "Vec<Slot>")]
pub struct Bank {
    slots: [Slot; SLOTS],
}

impl Default for Bank {
    fn default() -> Bank {
        Bank {
            slots: std::array::from_fn(|_| Slot::default()),
        }
    }
}

impl From<Vec<Slot>> for Bank {
    fn from(saved: Vec<Slot>) -> Bank {
        let mut bank = Bank::default();
        for (slot, s) in bank.slots.iter_mut().zip(saved) {
            *slot = Slot {
                volume: if s.volume.is_finite() {
                    s.volume.clamp(0.0, 1.0)
                } else {
                    1.0
                },
                ..s
            };
        }
        bank
    }
}

impl From<Bank> for Vec<Slot> {
    fn from(bank: Bank) -> Vec<Slot> {
        bank.slots.into()
    }
}

impl Bank {
    pub fn slot(&self, slot: SampleSlot) -> &Slot {
        &self.slots[slot.index()]
    }

    pub fn slot_mut(&mut self, slot: SampleSlot) -> &mut Slot {
        &mut self.slots[slot.index()]
    }

    /// The slots that have a file, with it.
    pub fn files(&self) -> impl Iterator<Item = (SampleSlot, &Path)> {
        SampleSlot::all().filter_map(|s| Some((s, self.slot(s).file.as_deref()?)))
    }
}

/// Things the sampler can be asked to do.
#[derive(Debug, Clone, PartialEq)]
pub enum SamplerCmd {
    /// Play the slot from its start (a pad).
    Play(SampleSlot),
    /// Stop it (SHIFT + pad).
    Stop(SampleSlot),
    /// The slot's volume, 0..1 (and save it).
    Volume(SampleSlot, f64),
    /// Play the slot round and round, or once through (and save it).
    Loop(SampleSlot, bool),
    /// Ask for a sound file to put in the slot.
    PickFile(SampleSlot),
    /// Put this file in the slot (and save it).
    Assign(SampleSlot, PathBuf),
    /// Put the track selected in the library in the slot.
    AssignSelected(SampleSlot),
    /// Empty the slot.
    Clear(SampleSlot),
}

impl SamplerCmd {
    /// The engine's command, for the ones it can take as they are.
    pub fn direct(&self) -> Option<EngineCommand> {
        match *self {
            SamplerCmd::Play(s) => Some(EngineCommand::Sampler(EngineSamplerCmd::Play(s))),
            SamplerCmd::Stop(s) => Some(EngineCommand::Sampler(EngineSamplerCmd::Stop(s))),
            _ => None,
        }
    }
}

/// The player's side of the sampler: each slot's sound and settings as the engine
/// has them (to give a new engine), and the newest load asked for on each (only that
/// one may put its sound in).
pub struct Session {
    pub slots: [SampleState; SLOTS],
    tickets: [u64; SLOTS],
}

impl Default for Session {
    fn default() -> Session {
        Session {
            slots: std::array::from_fn(|_| SampleState::default()),
            tickets: [0; SLOTS],
        }
    }
}

/// Why [`load`] didn't put its sound in: a newer load or a clear came meanwhile.
pub const SUPERSEDED: &str = "a newer sound replaced it";

/// Give the engine and the session the bank's settings (not its sounds: [`load`]).
pub fn set_bank(player: &Player, bank: &Bank) {
    for slot in SampleSlot::all() {
        let s = bank.slot(slot);
        set_volume(player, slot, s.volume);
        set_loop(player, slot, s.looping);
    }
}

/// Send to the engine, if audio is running (if not, the session has it for when it
/// starts).
fn send(inner: &mut super::player::Inner, cmd: EngineSamplerCmd) {
    if inner.engine.is_some()
        && let Err(e) = inner.send(EngineCommand::Sampler(cmd))
    {
        eprintln!("sampler: {e}");
    }
}

pub fn set_volume(player: &Player, slot: SampleSlot, volume: f64) {
    let mut inner = player.lock().unwrap();
    inner.sampler.slots[slot.index()].volume = volume;
    send(&mut inner, EngineSamplerCmd::Volume(slot, volume));
}

pub fn set_loop(player: &Player, slot: SampleSlot, on: bool) {
    let mut inner = player.lock().unwrap();
    inner.sampler.slots[slot.index()].looping = on;
    send(&mut inner, EngineSamplerCmd::Loop(slot, Set::from_bool(on)));
}

/// Note a new load for `slot`, before starting it. Returns its ticket for [`load`]:
/// an older load still decoding won't land after it.
pub fn request_load(player: &Player, slot: SampleSlot) -> u64 {
    let mut inner = player.lock().unwrap();
    let t = &mut inner.sampler.tickets[slot.index()];
    *t += 1;
    *t
}

/// Empty the slot (and drop any load still decoding for it).
pub fn clear(player: &Player, slot: SampleSlot) {
    let mut inner = player.lock().unwrap();
    inner.sampler.tickets[slot.index()] += 1;
    inner.sampler.slots[slot.index()].audio = None;
    send(&mut inner, EngineSamplerCmd::Load { slot, audio: None });
}

/// Decode `file` (its first [`MAX_SECS`]) and put it in `slot`, unless a newer load
/// (than `ticket`, from [`request_load`]) was asked for meanwhile: then it fails
/// with [`SUPERSEDED`]. Blocking: call it from a background thread.
pub fn load(player: &Player, slot: SampleSlot, file: &Path, ticket: u64) -> Result<(), String> {
    let current = |player: &Player| player.lock().unwrap().sampler.tickets[slot.index()] == ticket;
    if !current(player) {
        return Err(SUPERSEDED.into());
    }
    let (audio, _) = decode::decode(&Location::File(file.to_path_buf()))?;
    let audio = Arc::new(trimmed(audio));
    if audio.frames() == 0 {
        return Err("there's no sound in it".into());
    }
    let mut inner = player.lock().unwrap();
    if inner.sampler.tickets[slot.index()] != ticket {
        return Err(SUPERSEDED.into());
    }
    inner.sampler.slots[slot.index()].audio = Some(audio.clone());
    send(
        &mut inner,
        EngineSamplerCmd::Load {
            slot,
            audio: Some(audio),
        },
    );
    Ok(())
}

/// `audio`, cut off at [`MAX_SECS`].
fn trimmed(mut audio: TrackAudio) -> TrackAudio {
    let most = 2 * (MAX_SECS * audio.sample_rate as f64) as usize;
    if audio.samples.len() > most {
        audio.samples.truncate(most);
        audio.samples.shrink_to_fit();
    }
    audio
}

/// A slot's pad light: lit while there's a sound in it, flashing while it plays.
pub fn light(snap: &SampleSnap) -> Light {
    match (snap.loaded, snap.playing) {
        (true, true) => Light::Flash,
        (true, false) => Light::On,
        _ => Light::Off,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::player::testing::{running, wait};
    use crate::core::player::{new_player, start_engine};

    fn slot(i: usize) -> SampleSlot {
        SampleSlot::new(i).unwrap()
    }

    /// A WAV file of `secs` of a 440 Hz tone at 44.1 kHz, in a folder of its own.
    pub(crate) fn tone(name: &str, secs: f64) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("earshot-sampler-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Air Horn.wav");
        let rate = 44_100u32;
        let n = (secs * rate as f64) as u32;
        let mut bytes = Vec::with_capacity(44 + 4 * n as usize);
        bytes.extend(b"RIFF");
        bytes.extend((36 + 4 * n).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16u32.to_le_bytes());
        bytes.extend(1u16.to_le_bytes()); // PCM
        bytes.extend(2u16.to_le_bytes()); // stereo
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 4).to_le_bytes());
        bytes.extend(4u16.to_le_bytes());
        bytes.extend(16u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend((4 * n).to_le_bytes());
        for i in 0..n {
            let v = (2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate as f64).sin();
            let s = ((v * 0.5 * i16::MAX as f64) as i16).to_le_bytes();
            bytes.extend(s);
            bytes.extend(s);
        }
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn pads_map_to_slots_by_side() {
        assert_eq!(pad_slot(0, 0), Some(slot(0)));
        assert_eq!(pad_slot(0, 7), Some(slot(7)));
        assert_eq!(pad_slot(1, 0), Some(slot(8)));
        assert_eq!(pad_slot(1, 7), Some(slot(15)));
        // Decks 3 and 4 are the same hands.
        assert_eq!(pad_slot(2, 3), Some(slot(3)));
        assert_eq!(pad_slot(3, 3), Some(slot(11)));
        assert_eq!(pad_slot(0, 8), None);
    }

    #[test]
    fn the_bank_saves_and_reads_back() {
        let mut bank = Bank::default();
        *bank.slot_mut(slot(2)) = Slot {
            file: Some("/sounds/Air Horn.wav".into()),
            volume: 0.5,
            looping: true,
        };
        let json = serde_json::to_string(&bank).unwrap();
        let back: Bank = serde_json::from_str(&json).unwrap();
        assert_eq!(back, bank);
        assert_eq!(back.slot(slot(2)).name().as_deref(), Some("Air Horn"));
        assert_eq!(back.slot(slot(0)).name(), None);
        assert_eq!(back.files().count(), 1);
        // A short list, with fields missing and a volume out of range: the rest are
        // empty, at their defaults.
        let odd: Bank = serde_json::from_str(r#"[{"file": "/a.wav", "volume": 7}, {}]"#).unwrap();
        assert_eq!(odd.slot(slot(0)).volume, 1.0);
        assert_eq!(odd.slot(slot(0)).file.as_deref(), Some(Path::new("/a.wav")));
        assert_eq!(*odd.slot(slot(1)), Slot::default());
        assert_eq!(*odd.slot(slot(15)), Slot::default());
        // More than there are slots: the first 16.
        let many = vec![Slot::default(); SLOTS + 4];
        assert_eq!(Bank::from(many), Bank::default());
    }

    #[test]
    fn a_file_loads_into_its_slot_and_plays() {
        let p = running();
        let file = tone("load", 0.5);
        let ticket = request_load(&p, slot(4));
        load(&p, slot(4), &file, ticket).unwrap();
        let s = wait(&p, |s| s.sampler[4].loaded);
        assert!((s.sampler[4].duration - 0.5).abs() < 0.01);
        assert!(!s.sampler[4].playing);
        let play = SamplerCmd::Play(slot(4)).direct().unwrap();
        p.lock().unwrap().send(play).unwrap();
        wait(&p, |s| s.sampler[4].playing);
        // Once through: it stops by itself.
        wait(&p, |s| !s.sampler[4].playing);
        // Settings reach the engine; SHIFT + pad stops a loop.
        set_loop(&p, slot(4), true);
        set_volume(&p, slot(4), 0.25);
        p.lock()
            .unwrap()
            .send(SamplerCmd::Play(slot(4)).direct().unwrap())
            .unwrap();
        let s = wait(&p, |s| s.sampler[4].playing);
        assert!(s.sampler[4].looping && s.sampler[4].volume == 0.25);
        p.lock()
            .unwrap()
            .send(SamplerCmd::Stop(slot(4)).direct().unwrap())
            .unwrap();
        wait(&p, |s| !s.sampler[4].playing);
        // Cleared: gone from the engine and the session.
        clear(&p, slot(4));
        wait(&p, |s| !s.sampler[4].loaded);
        assert!(p.lock().unwrap().sampler.slots[4].audio.is_none());
        // The rest aren't engine commands.
        assert!(SamplerCmd::Clear(slot(4)).direct().is_none());
        assert!(SamplerCmd::Volume(slot(4), 0.5).direct().is_none());
    }

    #[test]
    fn a_stale_or_bad_load_changes_nothing() {
        let p = running();
        let file = tone("stale", 0.1);
        let old = request_load(&p, slot(0));
        let new = request_load(&p, slot(0));
        assert_eq!(load(&p, slot(0), &file, old).unwrap_err(), SUPERSEDED);
        assert!(p.lock().unwrap().sampler.slots[0].audio.is_none());
        // Cleared while decoding: it doesn't land either.
        clear(&p, slot(0));
        assert_eq!(load(&p, slot(0), &file, new).unwrap_err(), SUPERSEDED);
        // A file that isn't there says so.
        let t = request_load(&p, slot(0));
        let e = load(&p, slot(0), Path::new("/no/such/sound.wav"), t).unwrap_err();
        assert_ne!(e, SUPERSEDED);
        assert!(p.lock().unwrap().sampler.slots[0].audio.is_none());
    }

    #[test]
    fn long_files_are_cut_off() {
        let rate = 8_000;
        let long = TrackAudio {
            samples: vec![0.0; 2 * (MAX_SECS as usize + 30) * rate as usize],
            sample_rate: rate,
        };
        assert!((trimmed(long).duration() - MAX_SECS).abs() < 1e-9);
        let short = TrackAudio {
            samples: vec![0.0; 2 * rate as usize],
            sample_rate: rate,
        };
        assert_eq!(trimmed(short).frames(), rate as usize);
    }

    /// Sounds loaded before audio starts, and after an audio restart, are in the
    /// engine, with their settings.
    #[test]
    fn sounds_survive_an_audio_restart() {
        let p = new_player();
        let file = tone("restart", 0.2);
        // No engine yet: the session keeps it.
        let ticket = request_load(&p, slot(9));
        load(&p, slot(9), &file, ticket).unwrap();
        let mut bank = Bank::default();
        bank.slot_mut(slot(9)).looping = true;
        bank.slot_mut(slot(9)).volume = 0.5;
        set_bank(&p, &bank);
        let null = crate::core::player::testing::null();
        start_engine(&p, &null).unwrap();
        let s = wait(&p, |s| s.sampler[9].loaded);
        assert!(s.sampler[9].looping && s.sampler[9].volume == 0.5);
        start_engine(&p, &null).unwrap();
        let s = wait(&p, |s| s.sampler[9].loaded);
        assert!(s.sampler[9].looping && s.sampler[9].volume == 0.5);
        assert!(!s.sampler[8].loaded);
    }

    #[test]
    fn lights_show_loaded_and_playing() {
        let mut s = SampleSnap::default();
        assert_eq!(light(&s), Light::Off);
        s.loaded = true;
        assert_eq!(light(&s), Light::On);
        s.playing = true;
        assert_eq!(light(&s), Light::Flash);
    }
}
