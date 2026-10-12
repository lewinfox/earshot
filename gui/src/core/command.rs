//! The one set of things the app can be asked to do. Every input (on-screen controls,
//! the keymap, MIDI) produces [`Command`]s, and [`dispatch`] carries them out.
//!
//! Commands the engine can take as they are ([`direct`]) can go straight to it on a
//! thread's own ring ([`Lane`]): that's how MIDI reaches the engine without the player
//! lock. The rest need app state (a deck's beat jump step, the library's selection)
//! and run on the UI thread.

use std::sync::{Arc, Mutex};

use earshot_engine::{
    Command as EngineCommand, CommandSender, DeckCmd as EngineCmd, DeckId, ParamId, Routing, Set,
};

use super::bus::step_size;
use super::collections::{CollectionCmd, FlowCmd};
use super::edit::EditCmd;
use super::index::LibraryCmd;
use super::player::{self, AudioSettings, GridOp, Player};
use super::store::{Memory, Store, StoreCmd};
use super::tempo_lock::TempoLockCmd;

#[derive(Debug, Clone)]
pub enum Command {
    Deck(DeckId, DeckCmd),
    /// A knob or fader: EQ, volume, crossfader, …
    Param(ParamId, f64),
    Mixer(MixerCmd),
    Audio(AudioCmd),
    /// The headphone preview of a library track.
    Preview(PreviewCmd),
    View(ViewCmd),
    App(AppCmd),
    /// Library tracks: load, analyse, library folders. Run by the UI, like `View`.
    Library(LibraryCmd),
    /// Crates, playlists, smart playlists, Flows and their folders.
    Collection(CollectionCmd),
    /// The Flow on the canvas.
    Flow(FlowCmd),
    /// The tempo lock tool (`TempoLock::run`). Run by the UI, like `Library`.
    #[allow(dead_code)] // the Tempo lock tab isn't built yet (step 7, B4)
    TempoLock(TempoLockCmd),
    /// Metadata, ratings, tags and collection edits, which can be undone. Run by the
    /// UI (it keeps the undo history), like `Library`.
    Edit(EditCmd),
    /// Take back the last edit (`core::edit::History`).
    Undo,
    Redo,
}

#[derive(Debug, Clone)]
pub enum DeckCmd {
    /// Straight to the engine.
    Engine(EngineCmd),
    /// Beat jump by the deck's step.
    Jump {
        forward: bool,
    },
    SetJumpSize(f64),
    /// The size the loop button makes.
    SetLoopSize(f64),
    /// The loop button: end the loop (whatever its size by now), or start one of the
    /// deck's loop size.
    LoopButton,
    /// Scale the loop by this, or the next loop's size when not looping.
    ScaleLoop(f64),
    /// Go this far through the track (0..1; below 0 is the lead-in).
    SeekFraction(f64),
    Grid(GridOp),
    /// Put a library track on the deck. The UI runs it (it shows the track at once).
    Load(i64),
    /// Save the cue point (or the loop while looping) as a memory cue.
    SaveMemory,
    /// Go to a memory cue (it becomes the cue point) or start a memory loop.
    RecallMemory(Memory),
    /// Forget one of the loaded track's memory cues.
    DeleteMemory(i64),
    /// Name one of the loaded track's memory cues (`None` clears it).
    SetMemoryLabel(i64, Option<String>),
    /// Colour one of the loaded track's memory cues (0xRRGGBB; `None`: the default).
    SetMemoryColor(i64, Option<u32>),
    /// Name a hot cue of the loaded track (`None` clears it).
    SetHotcueLabel(u8, Option<String>),
}

#[derive(Debug, Clone, Copy)]
pub enum MixerCmd {
    /// The deck synced decks follow (`None`: each picks one).
    SyncLeader(Option<DeckId>),
}

#[derive(Debug, Clone)]
pub enum AudioCmd {
    /// Restart audio with another device or buffer size. The UI runs it (it keeps the
    /// settings and shows the result).
    Restart(AudioSettings),
    /// Change where the master and headphone buses go, live.
    Route(Routing),
}

#[derive(Debug, Clone, Copy)]
pub enum PreviewCmd {
    /// Play a track in the headphones, or pause / resume it if it's the one loaded.
    Toggle(i64),
    /// Move the preview of a track to this far in (0..1), starting it if needed.
    Seek(i64, f32),
    Stop(i64),
}

pub use super::view::ViewCmd;

/// The app's own housekeeping. Run by the UI.
#[derive(Debug, Clone, Copy)]
pub enum AppCmd {
    /// Close and reopen every MIDI input, e.g. after plugging the controller in.
    ReconnectMidi,
    /// Forget the analysis failures counted so far.
    DismissFailures,
}

impl From<(DeckId, EngineCmd)> for Command {
    fn from((deck, c): (DeckId, EngineCmd)) -> Command {
        Command::Deck(deck, DeckCmd::Engine(c))
    }
}

/// The engine command, for commands the engine can take as they are; otherwise the
/// command back, for [`dispatch`].
pub fn direct(cmd: Command) -> Result<EngineCommand, Command> {
    match cmd {
        Command::Deck(deck, DeckCmd::Engine(c)) => Ok(EngineCommand::Deck(deck, c)),
        Command::Param(p, v) => Ok(EngineCommand::Param(p, v)),
        other => Err(other),
    }
}

/// A thread's own command ring into the engine (see `Engine::sender`). The lock is only
/// shared with an audio restart, which puts the new engine's ring in.
#[derive(Clone, Default)]
pub struct Lane(Arc<Mutex<Option<CommandSender>>>);

impl Lane {
    pub fn send(&self, cmd: EngineCommand) -> Result<(), String> {
        match self.0.lock().unwrap().as_mut() {
            Some(s) => s.send(cmd),
            None => Err("audio is not running".into()),
        }
    }

    pub fn set(&self, sender: Option<CommandSender>) {
        *self.0.lock().unwrap() = sender;
    }
}

/// Carry out a command for the decks, mixer or engine. Returns the ones that belong
/// to the UI (views, loads, previews and audio restarts) for it to run.
pub fn dispatch(player: &Player, store: &Store, cmd: Command) -> Result<Option<Command>, String> {
    let cmd = match direct(cmd) {
        Ok(c) => return player.lock().unwrap().send(c).map(|_| None),
        Err(cmd) => cmd,
    };
    let (deck, cmd) = match cmd {
        Command::Deck(deck, c) => (deck, c),
        Command::Mixer(MixerCmd::SyncLeader(l)) => {
            return player
                .lock()
                .unwrap()
                .send(EngineCommand::SetSyncLeader(l))
                .map(|_| None);
        }
        Command::Audio(AudioCmd::Route(r)) => return player::set_routing(player, r).map(|_| None),
        other => return Ok(Some(other)),
    };
    let engine = |c| EngineCommand::Deck(deck, c);
    if let DeckCmd::Grid(op) = cmd {
        // Outside the lock: it takes it itself.
        return player::edit_grid(player, store, deck, op).map(|_| None);
    }
    let mut inner = player.lock().unwrap();
    let snap = inner.snapshot().map(|s| *s.deck(deck));
    let looping = snap.is_some_and(|s| s.loop_active);
    let session = inner.session(deck);
    let (jump_size, loop_size, track) = (session.jump_size, session.loop_size, session.track_id);
    match cmd {
        DeckCmd::Jump { forward } => {
            let beats = if forward { jump_size } else { -jump_size };
            inner.send(engine(EngineCmd::BeatJump(beats)))?;
        }
        DeckCmd::SetJumpSize(beats) if beats > 0.0 => inner.session_mut(deck).jump_size = beats,
        DeckCmd::SetLoopSize(beats) if beats > 0.0 => inner.session_mut(deck).loop_size = beats,
        DeckCmd::SetJumpSize(_) | DeckCmd::SetLoopSize(_) => {}
        DeckCmd::LoopButton => {
            let c = if looping {
                EngineCmd::Reloop(Set::Off)
            } else {
                EngineCmd::BeatLoop(loop_size)
            };
            inner.send(engine(c))?;
        }
        DeckCmd::ScaleLoop(factor) => {
            if looping {
                inner.send(engine(EngineCmd::LoopScale(factor)))?;
            } else {
                let dir = if factor < 1.0 { -1 } else { 1 };
                inner.session_mut(deck).loop_size = step_size(loop_size, dir);
            }
        }
        DeckCmd::SeekFraction(at) => {
            let duration = snap.map_or(0.0, |s| s.duration);
            inner.send(engine(EngineCmd::Seek(at.min(1.0) * duration)))?;
        }
        DeckCmd::SaveMemory => {
            let (Some(track), Some(d)) = (track, snap) else {
                return Ok(None);
            };
            let (start, end) = match (d.loop_active, d.loop_in, d.loop_out) {
                (true, Some(start), Some(end)) => (start, Some(end)),
                (true, _, _) => return Ok(None),
                (false, _, _) => (d.cue_point, None),
            };
            // The UI's list is updated from the `Cues` event that comes back.
            store.send(StoreCmd::AddMemory { track, start, end });
        }
        DeckCmd::RecallMemory(m) => match m.end {
            Some(end) => inner.send(engine(EngineCmd::SetLoop {
                start: m.start,
                end,
            }))?,
            None => {
                inner.send(engine(EngineCmd::SetCue(Some(m.start))))?;
                inner.send(engine(EngineCmd::Seek(m.start)))?;
            }
        },
        // These change what the UI shows from the `Cues` event that comes back.
        DeckCmd::DeleteMemory(id) => {
            if let Some(track) = track {
                store.send(StoreCmd::DeleteMemory { track, id });
            }
        }
        DeckCmd::SetMemoryLabel(id, label) => {
            if let Some(track) = track {
                store.send(StoreCmd::SetMemoryLabel { track, id, label });
            }
        }
        DeckCmd::SetMemoryColor(id, color) => {
            if let Some(track) = track {
                store.send(StoreCmd::SetMemoryColor { track, id, color });
            }
        }
        DeckCmd::SetHotcueLabel(slot, label) => {
            if let Some(track) = track {
                store.send(StoreCmd::SetHotcueLabel { track, slot, label });
            }
        }
        DeckCmd::Load(_) => return Ok(Some(Command::Deck(deck, cmd))),
        DeckCmd::Engine(_) | DeckCmd::Grid(_) => unreachable!("handled above"),
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::core::player::testing::{put_on, running, wait};
    use crate::core::store::FILE_NAME;

    fn store(name: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("earshot-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir.join(FILE_NAME), Arc::new(|_| {})).unwrap()
    }

    #[test]
    fn deck_commands_use_the_sessions_sizes() {
        let (p, store) = (running(), store("dispatch"));
        let a = DeckId::new(0).unwrap();
        put_on(&p, a, 1);
        let run = |c: DeckCmd| dispatch(&p, &store, Command::Deck(a, c)).unwrap();
        wait(&p, |s| s.decks[0].track_id == Some(1));

        // Jumps move by the session's step (the fallback grid is 0.5 s a beat).
        assert!(run(DeckCmd::SetJumpSize(8.0)).is_none());
        run(DeckCmd::Jump { forward: true });
        wait(&p, |s| (s.decks[0].position - 4.0).abs() < 0.01);
        run(DeckCmd::Jump { forward: false });
        wait(&p, |s| s.decks[0].position.abs() < 0.01);

        // Not looping: ½ steps the next loop's size down; the loop button uses it.
        run(DeckCmd::ScaleLoop(0.5));
        assert_eq!(p.lock().unwrap().session(a).loop_size, 2.0);
        run(DeckCmd::LoopButton);
        let s = wait(&p, |s| s.decks[0].loop_active);
        assert_eq!(s.decks[0].loop_out, Some(1.0), "2 beats");
        // Looping: ×2 doubles the loop itself, and the button ends it.
        run(DeckCmd::ScaleLoop(2.0));
        wait(&p, |s| s.decks[0].loop_out == Some(2.0));
        assert_eq!(p.lock().unwrap().session(a).loop_size, 2.0);
        run(DeckCmd::LoopButton);
        wait(&p, |s| !s.decks[0].loop_active);

        run(DeckCmd::SeekFraction(0.5));
        wait(&p, |s| (s.decks[0].position - 15.0).abs() < 0.01);
    }

    #[test]
    fn ui_commands_come_back() {
        let (p, store) = (running(), store("dispatch-ui"));
        let a = DeckId::new(1).unwrap();
        for c in [
            Command::View(ViewCmd::Browse(1)),
            Command::Deck(a, DeckCmd::Load(5)),
            Command::Preview(PreviewCmd::Stop(5)),
        ] {
            assert!(dispatch(&p, &store, c).unwrap().is_some());
        }
        assert!(dispatch(&p, &store, Command::Undo).unwrap().is_some());
        let toggle = Command::from((a, EngineCmd::Keylock(Set::Toggle)));
        assert!(dispatch(&p, &store, toggle).unwrap().is_none());
        wait(&p, |s| s.decks[1].keylock);
    }
}

impl From<LibraryCmd> for Command {
    fn from(c: LibraryCmd) -> Command {
        Command::Library(c)
    }
}

impl From<CollectionCmd> for Command {
    fn from(c: CollectionCmd) -> Command {
        Command::Collection(c)
    }
}

impl From<FlowCmd> for Command {
    fn from(c: FlowCmd) -> Command {
        Command::Flow(c)
    }
}

impl From<EditCmd> for Command {
    fn from(c: EditCmd) -> Command {
        Command::Edit(c)
    }
}

impl From<TempoLockCmd> for Command {
    fn from(c: TempoLockCmd) -> Command {
        Command::TempoLock(c)
    }
}

impl From<ViewCmd> for Command {
    fn from(c: ViewCmd) -> Command {
        Command::View(c)
    }
}

impl From<AppCmd> for Command {
    fn from(c: AppCmd) -> Command {
        Command::App(c)
    }
}
