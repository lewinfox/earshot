//! The writer: everything that changes the database, run only on the Store thread.
//! Each method is one transaction, and queues the [`StoreEvent`]s that describe what
//! changed; the thread sends them once the method returns.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::TrackTags;
use rusqlite::{Connection, OptionalExtension, params};

use super::read::{self, JOB_PREFIX, JobRow, source_rank, waveform_params};
use super::{
    CueKind, FlowCamera, GridSource, Result, StoreCmd, StoreError, StoreEvent, WAVEFORM_VERSION,
    bytes_path, now, path_bytes,
};
use crate::core::analysis::registry::{self, AnalyserId};
use crate::core::collections::{
    self, CollectionKind, FlowGraph, FlowNode, NodeId, Section, Spacing,
};
use crate::core::edit::Edit;
use crate::core::embeddings::{self, CLAP_MODEL, Embeddings};
use crate::core::hash::Hash;
use crate::core::keys::{parse_key, pitch_class};
use crate::core::query;

mod edits;

/// A file the scan found that is new, or whose size or modified time changed.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size: i64,
    pub mtime_ns: i64,
    pub tags: TrackTags,
    /// From the file's `TXXX:AUDIO_SHA256`, when it has one.
    pub tag_hash: Option<Hash>,
}

/// Everything one analysis job found, written in one transaction.
#[derive(Debug)]
pub struct AnalysisSave {
    pub track: i64,
    /// e.g. the worker's detector ("static", "beat_this").
    pub analyser: String,
    /// "static" or "dynamic".
    pub mode: &'static str,
    pub started_at: i64,
    /// The job failed: the run is recorded, with whatever was measured before.
    pub error: Option<String>,
    /// energy, lufs, peak, bpm_min, bpm_max, drifts …
    pub features: Vec<(&'static str, f64)>,
    pub waveform: Option<Vec<u8>>,
    pub grid: Option<GridSave>,
    /// One `analysis_results` row per analyser that ran: (name, version, error).
    pub results: Vec<(&'static str, u32, Option<String>)>,
}

#[derive(Debug)]
pub struct GridSave {
    /// Saved as its model; the track's BPM comes from it ([`grid_bpm`]).
    pub grid: Beatgrid,
    pub source: GridSource,
    /// The active grid when the job started. The new grid only becomes the active
    /// one if that hasn't changed since (compare-and-set), so a grid fixed by hand
    /// while the job ran is kept.
    pub expected_active: Option<i64>,
    /// The user asked for this analysis: it may replace a hand-fixed grid (which
    /// stays in the database as an older row).
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SaveOutcome {
    /// The new grid is now the active one (so decks should get it).
    pub switched: bool,
    /// The track's effective BPM and grid source afterwards.
    pub bpm: Option<f64>,
    pub source: Option<GridSource>,
}

pub struct Writer {
    conn: Connection,
    events: Vec<StoreEvent>,
    /// The play session plays are saved in (`StoreCmd::StartSession`).
    session: Option<i64>,
    /// Every library track's [`CLAP_MODEL`] vector, loaded here and kept in step with
    /// what this writer saves (`Store::embeddings` shares it).
    embeddings: Arc<RwLock<Embeddings>>,
}

impl Writer {
    pub(super) fn new(conn: Connection) -> Writer {
        let mut block = Embeddings::default();
        match read::embeddings(&conn, CLAP_MODEL, None) {
            Ok(rows) => {
                for (id, v) in rows {
                    block.insert(id, &v);
                }
            }
            Err(e) => eprintln!("loading the embeddings: {e}"),
        }
        Writer {
            conn,
            events: vec![],
            session: None,
            embeddings: Arc::new(RwLock::new(block)),
        }
    }

    /// The in-memory embeddings, shared.
    pub fn embeddings(&self) -> Arc<RwLock<Embeddings>> {
        self.embeddings.clone()
    }

    /// Save a track's embedding from `model`. A [`CLAP_MODEL`] one goes into the
    /// in-memory block too.
    pub fn save_embedding(&mut self, track: i64, model: &str, vector: &[f32]) -> Result<()> {
        if vector.is_empty() || vector.iter().any(|x| !x.is_finite()) {
            return Err(StoreError::Refused("an embedding must be finite numbers"));
        }
        self.conn
            .prepare_cached(
                "INSERT INTO embeddings(track_id, model, dim, vector) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(track_id, model) DO UPDATE SET dim = excluded.dim,
                   vector = excluded.vector",
            )?
            .execute(params![
                track,
                model,
                vector.len() as i64,
                embeddings::to_blob(vector)
            ])?;
        if model == CLAP_MODEL {
            self.refresh_embeddings(&[track])?;
        }
        Ok(())
    }

    /// Bring these tracks' rows of the in-memory block in line with the database:
    /// a library track's vector in, a removed or merged-away track's out.
    pub(super) fn refresh_embeddings(&mut self, tracks: &[i64]) -> Result<()> {
        let found = read::embeddings(&self.conn, CLAP_MODEL, Some(tracks))?;
        let mut block = self.embeddings.write().unwrap();
        for &t in tracks {
            match found.iter().find(|f| f.0 == t) {
                Some((_, v)) if block.insert(t, v) => {}
                _ => block.remove(t),
            }
        }
        Ok(())
    }

    pub fn emit(&mut self, e: StoreEvent) {
        self.events.push(e);
    }

    pub fn take_events(&mut self) -> Vec<StoreEvent> {
        std::mem::take(&mut self.events)
    }

    /// The active grid's row id and source. On the writer thread, so it's ordered
    /// with the writes (see `core::analysis`).
    pub fn active_grid(&self, track: i64) -> Result<Option<(i64, GridSource)>> {
        read::active_grid(&self.conn, track)
    }

    #[cfg(test)]
    pub(super) fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn apply(&mut self, cmd: StoreCmd) -> Result<()> {
        match cmd {
            StoreCmd::ListRoots => self.emit_roots(),
            StoreCmd::AddRoots(paths) => self.add_roots(&paths),
            StoreCmd::RemoveRoot(path) => self.remove_root(&path),
            StoreCmd::SaveUserGrid { track, grid } => self.save_user_grid(track, &grid).map(|_| ()),
            StoreCmd::SaveWaveform { track, data } => self.save_waveform(track, &data),
            StoreCmd::SaveFeatures { track, features } => {
                self.save_measured(track, &features)?;
                self.tracks_changed(&[track])
            }
            StoreCmd::SaveJobs { track, jobs } => self.save_jobs(track, &jobs),
            StoreCmd::SetSetting { key, value } => self.set_setting(&key, &value),
            StoreCmd::StartSession => self.start_session().map(|_| ()),
            StoreCmd::SavePlay {
                track,
                deck,
                started_at,
            } => self.save_play(track, deck, started_at),
            StoreCmd::EndPlay { deck } => self.end_play(deck, now()),
            StoreCmd::ListSessions => {
                let s = read::sessions(&self.conn)?;
                self.emit(StoreEvent::Sessions(s));
                Ok(())
            }
            StoreCmd::SetMeta {
                track,
                field,
                value,
            } => self.set_meta(track, &field, "user", value.as_deref()),
            StoreCmd::SaveEmbedding {
                track,
                model,
                vector,
            } => self.save_embedding(track, &model, &vector),
            StoreCmd::SetContentHash { file, hash } => {
                self.set_content_hash(file, &hash).map(|_| ())
            }
            StoreCmd::SaveHotcues { track, cues } => {
                self.save_hotcues(track, &cues)?;
                self.emit_cues(track)
            }
            StoreCmd::SetHotcueLabel { track, slot, label } => {
                self.set_hotcue_label(track, slot, label.as_deref())
            }
            StoreCmd::AddMemory { track, start, end } => self.add_memory(track, start, end),
            StoreCmd::DeleteMemory { track, id } => self.delete_memory(track, id),
            StoreCmd::SetMemoryLabel { track, id, label } => {
                self.set_memory_label(track, id, label.as_deref())
            }
            StoreCmd::SetMemoryColor { track, id, color } => {
                self.set_memory_color(track, id, color)
            }
            StoreCmd::ListCollections => self.emit_collections(),
            StoreCmd::LoadFlow(id) => {
                let f = read::flow(&self.conn, id)?;
                let camera = match f {
                    Some(_) => read::flow_camera(&self.conn, id)?,
                    None => None,
                };
                self.emit(StoreEvent::FlowLoaded(f, camera));
                Ok(())
            }
            StoreCmd::SaveFlowCamera { id, camera } => self.save_flow_camera(id, camera),
            StoreCmd::Create {
                id,
                parent,
                kind,
                name,
                query,
            } => self.create_collection(id, parent, kind, &name, query.as_deref()),
            StoreCmd::CreateFolder {
                id,
                parent,
                section,
                name,
            } => self.create_folder(id, parent, section, &name),
            StoreCmd::Rename { id, name } => self.rename(id, &name),
            StoreCmd::Move { id, parent } => self.move_to(id, parent),
            StoreCmd::Delete(id) => self.delete(id),
            StoreCmd::Duplicate { from, to } => self.duplicate(from, to),
            StoreCmd::SaveFlow { id, graph } => self.save_flow(id, &graph),
            StoreCmd::AddTracks { id, tracks } => self.add_tracks(id, &tracks),
            StoreCmd::RemoveTracks { id, tracks } => self.remove_tracks(id, &tracks),
            StoreCmd::SetQuery { id, query } => self.set_query(id, &query),
            StoreCmd::Edit(cmd) => self.edit(cmd).map(|_| ()),
            StoreCmd::ApplyEdit(e) => self.apply_edit(&e),
            StoreCmd::ExportPath {
                flow,
                nodes,
                playlist,
                name,
            } => self.export_path(flow, &nodes, playlist, &name),
        }
    }

    fn tracks_changed(&mut self, ids: &[i64]) -> Result<()> {
        let t = read::tracks(&self.conn, Some(ids))?;
        self.emit(StoreEvent::TracksChanged(t));
        Ok(())
    }

    pub(super) fn emit_library(&mut self) -> Result<()> {
        let t = read::tracks(&self.conn, None)?;
        self.emit(StoreEvent::Library(t));
        Ok(())
    }

    // ---- library roots ----------------------------------------------------------------

    pub fn roots(&self) -> Result<Vec<PathBuf>> {
        Ok(read::roots(&self.conn)?
            .into_iter()
            .map(bytes_path)
            .collect())
    }

    fn emit_roots(&mut self) -> Result<()> {
        let r = self
            .roots()?
            .into_iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        self.emit(StoreEvent::Roots(r));
        Ok(())
    }

    pub fn add_roots(&mut self, paths: &[PathBuf]) -> Result<()> {
        for p in paths {
            if !p.is_dir() {
                self.emit(StoreEvent::Error(StoreError::Refused(
                    "that is not a folder",
                )));
                continue;
            }
            let mut b = path_bytes(p);
            while b.len() > 1 && b.ends_with(b"/") {
                b.pop();
            }
            self.conn.execute(
                "INSERT OR IGNORE INTO library_roots(path, added_at) VALUES (?1, ?2)",
                params![b, now()],
            )?;
        }
        self.emit_roots()
    }

    /// Tracks stay in the database (keeping their analysis), but aren't listed.
    pub fn remove_root(&mut self, path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM library_roots WHERE path = ?1",
            [path_bytes(std::path::Path::new(path))],
        )?;
        self.emit_roots()?;
        self.emit_library()
    }

    // ---- scanning and identity ---------------------------------------------------------

    /// Record a scan of `roots`: `found` is every audio file under them, `changed`
    /// the new or modified ones with their tags. Files under the roots that weren't
    /// found are marked missing (not deleted). A new file whose stamped hash matches
    /// a track whose files are all missing becomes that track's file (it was moved).
    /// Only tag-sourced metadata is written, so the user's edits survive.
    pub fn apply_scan(
        &mut self,
        roots: &[PathBuf],
        found: &[PathBuf],
        changed: &[ScannedFile],
    ) -> Result<()> {
        let t = now();
        let tx = self.conn.transaction()?;
        tx.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS seen(path BLOB PRIMARY KEY); DELETE FROM seen;",
        )?;
        {
            let mut st = tx.prepare("INSERT OR IGNORE INTO seen(path) VALUES (?1)")?;
            for p in found {
                st.execute([path_bytes(p)])?;
            }
        }
        tx.execute(
            "UPDATE files SET missing_since = NULL
             WHERE missing_since IS NOT NULL AND path IN (SELECT path FROM seen)",
            [],
        )?;
        for root in roots {
            let mut prefix = path_bytes(root);
            if !prefix.ends_with(b"/") {
                prefix.push(b'/');
            }
            tx.execute(
                "UPDATE files SET missing_since = ?1
                 WHERE missing_since IS NULL AND substr(path, 1, ?2) = ?3
                   AND path NOT IN (SELECT path FROM seen)",
                params![t, prefix.len() as i64, prefix],
            )?;
        }
        for f in changed {
            let bytes = path_bytes(&f.path);
            let existing: Option<i64> = tx
                .query_row(
                    "SELECT track_id FROM files WHERE path = ?1",
                    [&bytes],
                    |r| r.get(0),
                )
                .optional()?;
            let track = match existing {
                Some(track) => {
                    // Changed on disk: its stamped hash (or none, to work out again).
                    tx.execute(
                        "UPDATE files SET size = ?2, mtime_ns = ?3, content_hash = ?4,
                           missing_since = NULL WHERE path = ?1",
                        params![bytes, f.size, f.mtime_ns, f.tag_hash.map(|h| h.to_vec())],
                    )?;
                    track
                }
                None => {
                    let moved = match &f.tag_hash {
                        Some(h) => orphan_with_hash(&tx, h, None)?,
                        None => None,
                    };
                    let track = match moved {
                        Some(track) => track,
                        None => {
                            tx.execute("INSERT INTO tracks(added_at) VALUES (?1)", [t])?;
                            tx.last_insert_rowid()
                        }
                    };
                    tx.execute(
                        "INSERT INTO files(track_id, path, size, mtime_ns, content_hash)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            track,
                            bytes,
                            f.size,
                            f.mtime_ns,
                            f.tag_hash.map(|h| h.to_vec())
                        ],
                    )?;
                    track
                }
            };
            if let Some(d) = f.tags.duration {
                tx.execute(
                    "UPDATE tracks SET duration_s = ?2 WHERE id = ?1",
                    params![track, d],
                )?;
            }
            write_tag_meta(&tx, track, &f.tags, t)?;
        }
        tx.commit()?;
        self.emit_library()
    }

    /// Record a file's audio hash, worked out in the background. If another track has
    /// the same audio and none of its files are present any more, this file is that
    /// track moved: the two are merged, keeping the older track (its analysis, cues
    /// and Flow places). Returns (merged away, kept) when that happens.
    pub fn set_content_hash(&mut self, file: i64, hash: &Hash) -> Result<Option<(i64, i64)>> {
        let tx = self.conn.transaction()?;
        let track: Option<i64> = tx
            .query_row("SELECT track_id FROM files WHERE id = ?1", [file], |r| {
                r.get(0)
            })
            .optional()?;
        let Some(track) = track else {
            return Ok(None); // gone since
        };
        tx.execute(
            "UPDATE files SET content_hash = ?2 WHERE id = ?1",
            params![file, hash.to_vec()],
        )?;
        let merged = match orphan_with_hash(&tx, hash, Some(track))? {
            Some(keep) => {
                merge(&tx, track, keep)?;
                Some((track, keep))
            }
            None => None,
        };
        // Results made before the hash was known were made from this audio.
        tx.execute(
            "UPDATE analysis_results SET content_hash = ?2
             WHERE track_id = ?1 AND content_hash IS NULL",
            params![merged.map_or(track, |m| m.1), hash.to_vec()],
        )?;
        tx.commit()?;
        if let Some((from, into)) = merged {
            self.refresh_embeddings(&[from, into])?;
            self.emit_library()?;
            self.emit_collections()?;
        }
        Ok(merged)
    }

    // ---- metadata ------------------------------------------------------------------------

    /// Set (or with `None` clear) one field from one source.
    pub fn set_meta(
        &mut self,
        track: i64,
        field: &str,
        source: &str,
        value: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        put_meta(&tx, track, field, source, value, now())?;
        reindex(&tx, track)?;
        tx.commit()?;
        self.tracks_changed(&[track])
    }

    // ---- analysis results --------------------------------------------------------------

    pub fn save_analysis(&mut self, s: AnalysisSave) -> Result<SaveOutcome> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO analysis_runs(track_id, analyser, mode, started_at, finished_at, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![s.track, s.analyser, s.mode, s.started_at, now(), s.error],
        )?;
        let run = tx.last_insert_rowid();
        save_features(&tx, s.track, &s.features, Some(run))?;
        for (analyser, version, error) in &s.results {
            put_result(
                &tx,
                s.track,
                analyser,
                *version,
                Some(run),
                error.as_deref(),
            )?;
        }
        let preview = match &s.waveform {
            Some(w) => Some(put_waveform(&tx, s.track, w)?),
            None => None,
        };
        let mut switched = false;
        if let Some(g) = &s.grid {
            let id = insert_grid(&tx, s.track, &g.grid, g.source, Some(run))?;
            let current = read::active_grid(&tx, s.track)?;
            let hand_fixed = current.is_some_and(|(_, src)| src == GridSource::User);
            // Compare-and-set: only if nobody changed the grid while the job ran, and
            // never over a hand-fixed grid unless the user asked for this analysis.
            if current.map(|c| c.0) == g.expected_active && (g.force || !hand_fixed) {
                activate_grid(&tx, s.track, id, grid_bpm(&g.grid))?;
                switched = true;
            }
        }
        let (bpm, source) = effective(&tx, s.track)?;
        tx.commit()?;
        if let Some(p) = preview {
            self.emit(StoreEvent::Previews(vec![(s.track, p)]));
        }
        self.tracks_changed(&[s.track])?;
        Ok(SaveOutcome {
            switched,
            bpm,
            source,
        })
    }

    /// A grid fixed by hand: a new row, made active, with the track's BPM from it.
    /// The grid it replaces stays, so it can be gone back to.
    /// Reported as an undoable edit (`Edit::Grid`).
    pub fn save_user_grid(&mut self, track: i64, grid: &Beatgrid) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let old = read::active_grid(&tx, track)?.map(|g| g.0);
        let id = insert_grid(&tx, track, grid, GridSource::User, None)?;
        activate_grid(&tx, track, id, grid_bpm(grid))?;
        tx.commit()?;
        self.tracks_changed(&[track])?;
        self.emit(StoreEvent::Edited(Edit::Grid {
            track,
            old,
            new: Some(id),
        }));
        Ok(id)
    }

    /// A waveform worked out outside analysis (on load): it counts as the waveform
    /// analyser's result.
    pub fn save_waveform(&mut self, track: i64, data: &[u8]) -> Result<()> {
        let tx = self.conn.transaction()?;
        let p = put_waveform(&tx, track, data)?;
        let spec = AnalyserId::Waveform.spec();
        put_result(&tx, track, spec.id.as_str(), spec.version, None, None)?;
        tx.commit()?;
        self.emit(StoreEvent::Previews(vec![(track, p)]));
        Ok(())
    }

    /// Numbers measured outside analysis (loudness on load). An analyser whose
    /// features are all here counts as having run.
    pub fn save_measured(&mut self, track: i64, features: &[(&'static str, f64)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        save_features(&tx, track, features, None)?;
        let names: Vec<&str> = features.iter().map(|f| f.0).collect();
        for spec in registry::measured_by(&names) {
            put_result(&tx, track, spec.id.as_str(), spec.version, None, None)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Replace what's queued for a track (`jobs`, kind `analyse:<analyser>`).
    pub fn save_jobs(&mut self, track: i64, jobs: &[JobRow]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM jobs WHERE track_id = ?1 AND kind LIKE ?2 || '%'",
            params![track, JOB_PREFIX],
        )?;
        let mut st = tx.prepare_cached(
            "INSERT INTO jobs(kind, track_id, status, created_at, want, params)
             SELECT ?1, ?2, 'queued', ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM tracks WHERE id = ?2)",
        )?;
        for j in jobs {
            debug_assert_eq!(j.track, track);
            let kind = format!("{JOB_PREFIX}{}", j.analyser);
            st.execute(params![kind, track, now(), j.want, j.params])?;
        }
        drop(st);
        tx.commit()?;
        Ok(())
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- play history -------------------------------------------------------------------

    /// Close any session left open at its last play (or its start), and open a new
    /// one. Returns its id.
    pub fn start_session(&mut self) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE sessions SET ended_at = max(started_at, coalesce(
                 (SELECT max(coalesce(ended_at, started_at)) FROM plays
                  WHERE session_id = sessions.id), started_at))
             WHERE ended_at IS NULL",
            [],
        )?;
        tx.execute("INSERT INTO sessions(started_at) VALUES (?1)", [now()])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        self.session = Some(id);
        Ok(id)
    }

    pub fn save_play(&mut self, track: i64, deck: usize, started_at: i64) -> Result<()> {
        let session = match self.session {
            Some(s) => s,
            None => self.start_session()?,
        };
        // A track removed meanwhile has nothing to record against.
        self.conn.execute(
            "INSERT INTO plays(session_id, track_id, deck, started_at, counted)
             SELECT ?1, id, ?3, ?4, 1 FROM tracks WHERE id = ?2",
            params![session, track, deck as i64, started_at],
        )?;
        // Its play count and last play.
        self.tracks_changed(&[track])
    }

    pub fn end_play(&mut self, deck: usize, at: i64) -> Result<()> {
        let Some(session) = self.session else {
            return Ok(());
        };
        self.conn.execute(
            "UPDATE plays SET ended_at = ?3
             WHERE session_id = ?1 AND deck = ?2 AND ended_at IS NULL",
            params![session, deck as i64, at],
        )?;
        Ok(())
    }

    // ---- cues --------------------------------------------------------------------------

    /// Save a track's hot cue times, pad by pad. The engine only knows times, so a pad
    /// that is set keeps its row (and so its label) with the new time; a pad that
    /// isn't loses its row, label and all.
    pub fn save_hotcues(&mut self, track: i64, cues: &[Option<f64>]) -> Result<()> {
        let tx = self.conn.transaction()?;
        let hot = CueKind::Hot.as_i64();
        tx.execute(
            "DELETE FROM cues WHERE track_id = ?1 AND kind = ?2 AND slot >= ?3",
            params![track, hot, cues.len() as i64],
        )?;
        for (slot, t) in cues.iter().enumerate() {
            match t {
                Some(t) => tx.execute(
                    "INSERT INTO cues(track_id, kind, slot, start_s) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (track_id, kind, slot) DO UPDATE SET start_s = excluded.start_s",
                    params![track, hot, slot as i64, t],
                )?,
                None => tx.execute(
                    "DELETE FROM cues WHERE track_id = ?1 AND kind = ?2 AND slot = ?3",
                    params![track, hot, slot as i64],
                )?,
            };
        }
        tx.commit()?;
        Ok(())
    }

    /// Name a hot cue that is set (a pad that isn't has no row to name). Empty is no
    /// name.
    pub fn set_hotcue_label(&mut self, track: i64, slot: u8, label: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE cues SET label = ?4 WHERE track_id = ?1 AND kind = ?2 AND slot = ?3",
            params![track, CueKind::Hot.as_i64(), slot as i64, tidy(label)],
        )?;
        self.emit_cues(track)
    }

    /// Save a memory cue (or loop, with `end`), unless the same one is already saved.
    pub fn add_memory(&mut self, track: i64, start: f64, end: Option<f64>) -> Result<()> {
        let same = read::memories(&self.conn, track)?.iter().any(|m| {
            (m.start - start).abs() < 0.005
                && match (m.end, end) {
                    (None, None) => true,
                    (Some(a), Some(b)) => (a - b).abs() < 0.005,
                    _ => false,
                }
        });
        if !same {
            let kind = if end.is_some() {
                CueKind::Loop
            } else {
                CueKind::Memory
            };
            self.conn.execute(
                "INSERT INTO cues(track_id, kind, start_s, end_s) VALUES (?1, ?2, ?3, ?4)",
                params![track, kind.as_i64(), start, end],
            )?;
        }
        self.emit_cues(track)
    }

    pub fn delete_memory(&mut self, track: i64, id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM cues WHERE id = ?1 AND track_id = ?2 AND kind IN (?3, ?4)",
            params![id, track, CueKind::Memory.as_i64(), CueKind::Loop.as_i64()],
        )?;
        self.emit_cues(track)
    }

    /// Name a memory cue or loop. Empty is no name.
    pub fn set_memory_label(&mut self, track: i64, id: i64, label: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE cues SET label = ?3 WHERE id = ?1 AND track_id = ?2 AND kind IN (?4, ?5)",
            params![
                id,
                track,
                tidy(label),
                CueKind::Memory.as_i64(),
                CueKind::Loop.as_i64()
            ],
        )?;
        self.emit_cues(track)
    }

    /// Colour a memory cue or loop (0xRRGGBB).
    pub fn set_memory_color(&mut self, track: i64, id: i64, color: Option<u32>) -> Result<()> {
        self.conn.execute(
            "UPDATE cues SET color = ?3 WHERE id = ?1 AND track_id = ?2 AND kind IN (?4, ?5)",
            params![
                id,
                track,
                color,
                CueKind::Memory.as_i64(),
                CueKind::Loop.as_i64()
            ],
        )?;
        self.emit_cues(track)
    }

    fn emit_cues(&mut self, track: i64) -> Result<()> {
        let cues = read::cues(&self.conn, track)?;
        self.emit(StoreEvent::Cues { track, cues });
        Ok(())
    }

    // ---- collections ----------------------------------------------------------------------

    pub(super) fn emit_collections(&mut self) -> Result<()> {
        let c = read::collections(&self.conn)?;
        self.emit(StoreEvent::Collections(c));
        Ok(())
    }

    /// `parent` is a folder that holds `section`'s things (or the top level).
    pub(super) fn check_folder(&self, parent: Option<i64>, section: Section) -> Result<()> {
        if let Some(p) = parent {
            match read::kind_and_section(&self.conn, p)? {
                Some((CollectionKind::Folder, s)) if s == section => {}
                Some((CollectionKind::Folder, _)) => {
                    return Err(StoreError::Refused(
                        "that folder holds another kind of thing",
                    ));
                }
                Some(_) => return Err(StoreError::Refused("only folders can hold things")),
                None => return Err(StoreError::NoCollection(p)),
            }
        }
        Ok(())
    }

    /// A crate, playlist, smart playlist or Flow, in its kind's section. A folder
    /// made here is in its parent's section, or at the top level, Playlists' (see
    /// [`Writer::create_folder`]).
    pub fn create_collection(
        &mut self,
        id: i64,
        parent: Option<i64>,
        kind: CollectionKind,
        name: &str,
        query: Option<&str>,
    ) -> Result<()> {
        let section = match (kind.section(), parent) {
            (Some(s), _) => s,
            (None, Some(p)) => {
                read::kind_and_section(&self.conn, p)?
                    .ok_or(StoreError::NoCollection(p))?
                    .1
            }
            (None, None) => Section::Playlists,
        };
        self.insert_collection(id, parent, kind, section, name, query)
    }

    /// A folder in a section's tree.
    pub fn create_folder(
        &mut self,
        id: i64,
        parent: Option<i64>,
        section: Section,
        name: &str,
    ) -> Result<()> {
        self.insert_collection(id, parent, CollectionKind::Folder, section, name, None)
    }

    fn insert_collection(
        &mut self,
        id: i64,
        parent: Option<i64>,
        kind: CollectionKind,
        section: Section,
        name: &str,
        query: Option<&str>,
    ) -> Result<()> {
        self.check_folder(parent, section)?;
        self.conn.execute(
            "INSERT INTO collections(id, parent_id, kind, section, name, query, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![id, parent, kind.as_str(), section.as_str(), name, query, now()],
        )?;
        self.emit_collections()
    }

    pub fn rename(&mut self, id: i64, name: &str) -> Result<()> {
        let n = self.conn.execute(
            "UPDATE collections SET name = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, name, now()],
        )?;
        if n == 0 {
            return Err(StoreError::NoCollection(id));
        }
        self.emit_collections()
    }

    /// Into a folder of its section, or to the top level. Refused if `parent` is
    /// inside `id`.
    pub fn move_to(&mut self, id: i64, parent: Option<i64>) -> Result<()> {
        let (_, section) =
            read::kind_and_section(&self.conn, id)?.ok_or(StoreError::NoCollection(id))?;
        self.check_folder(parent, section)?;
        if let Some(p) = parent {
            let parents: HashMap<i64, Option<i64>> = self
                .conn
                .prepare("SELECT id, parent_id FROM collections WHERE kind = 'folder'")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            if collections::is_within(p, id, |f| parents.get(&f).copied().flatten()) {
                return Err(StoreError::Refused("can't move a folder into itself"));
            }
        }
        self.conn.execute(
            "UPDATE collections SET parent_id = ?2 WHERE id = ?1",
            params![id, parent],
        )?;
        self.emit_collections()
    }

    /// Delete a collection and, for a folder, everything in it.
    pub fn delete(&mut self, id: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let gone = {
            let mut st = tx.prepare(
                "WITH RECURSIVE under(id) AS (
                   SELECT id FROM collections WHERE id = ?1
                   UNION ALL SELECT c.id FROM collections c JOIN under u ON c.parent_id = u.id)
                 SELECT id FROM under",
            )?;
            st.query_map([id], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<i64>>>()?
        };
        tx.execute("DELETE FROM collections WHERE id = ?1", [id])?;
        tx.commit()?;
        self.emit(StoreEvent::Deleted(gone));
        self.emit_collections()
    }

    /// A copy of a crate, playlist, smart playlist or Flow beside it, named
    /// "<name> copy". (Not folders.)
    pub fn duplicate(&mut self, from: i64, to: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "INSERT INTO collections(id, parent_id, kind, section, name, query, source_flow_id, created_at, updated_at)
             SELECT ?2, parent_id, kind, section, name || ' copy', query, source_flow_id, ?3, ?3
             FROM collections WHERE id = ?1 AND kind <> 'folder'",
            params![from, to, now()],
        )?;
        if n == 0 {
            return Err(StoreError::NoCollection(from));
        }
        for sql in [
            "INSERT INTO crate_tracks(collection_id, track_id, added_at)
             SELECT ?2, track_id, added_at FROM crate_tracks WHERE collection_id = ?1",
            "INSERT INTO playlist_entries(collection_id, position, track_id)
             SELECT ?2, position, track_id FROM playlist_entries WHERE collection_id = ?1",
            "INSERT INTO flow_nodes(collection_id, id, track_id, x, y)
             SELECT ?2, id, track_id, x, y FROM flow_nodes WHERE collection_id = ?1",
            "INSERT INTO flow_edges(collection_id, from_node, to_node)
             SELECT ?2, from_node, to_node FROM flow_edges WHERE collection_id = ?1",
        ] {
            tx.execute(sql, params![from, to])?;
        }
        tx.commit()?;
        self.emit_collections()
    }

    /// Save a Flow, writing only what changed. Refused if the links loop.
    pub fn save_flow(&mut self, id: i64, graph: &FlowGraph) -> Result<()> {
        // `new` drops links to cards that aren't there.
        let graph = FlowGraph::new(graph.nodes.clone(), graph.edges.clone());
        if collections::check_acyclic(&graph.edges).is_err() {
            return Err(StoreError::Refused("a Flow can't loop back on itself"));
        }
        let tx = self.conn.transaction()?;
        if read::kind(&tx, id)? != Some(CollectionKind::Flow) {
            return Err(StoreError::NoCollection(id));
        }
        write_graph(&tx, id, &graph)?;
        tx.commit()?;
        self.emit_collections()
    }

    /// Save one path through a Flow (card ids, in order) as a new playlist, at the top
    /// of the Playlists tree (the Flow's folder only holds Flows). The playlist is a
    /// snapshot: later changes to the Flow don't change it.
    pub fn export_path(
        &mut self,
        flow: i64,
        nodes: &[NodeId],
        playlist: i64,
        name: &str,
    ) -> Result<()> {
        let graph = read::flow_graph(&self.conn, flow)?;
        let Some(tracks) = graph.bake(nodes) else {
            return Err(StoreError::Refused(
                "those cards aren't a path through the Flow",
            ));
        };
        self.create_playlist(playlist, None, name, &tracks, Some(flow))
    }

    /// A playlist of these tracks, in order (a track may repeat).
    pub fn create_playlist(
        &mut self,
        id: i64,
        parent: Option<i64>,
        name: &str,
        tracks: &[i64],
        source_flow: Option<i64>,
    ) -> Result<()> {
        self.check_folder(parent, Section::Playlists)?;
        let tx = self.conn.transaction()?;
        let t = now();
        tx.execute(
            "INSERT INTO collections(id, parent_id, kind, section, name, source_flow_id, created_at, updated_at)
             VALUES (?1, ?2, 'playlist', 'playlists', ?3, ?4, ?5, ?5)",
            params![id, parent, name, source_flow, t],
        )?;
        for (i, track) in tracks.iter().enumerate() {
            tx.execute(
                "INSERT INTO playlist_entries(collection_id, position, track_id) VALUES (?1, ?2, ?3)",
                params![id, i as i64, track],
            )?;
        }
        tx.commit()?;
        self.emit_collections()
    }

    /// Add tracks: to a crate (a track already in it is left as it is), the end of a
    /// playlist, or a Flow as new starting cards.
    pub fn add_tracks(&mut self, id: i64, tracks: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        let t = now();
        match read::kind(&tx, id)? {
            Some(CollectionKind::Crate) => {
                for track in tracks {
                    tx.execute(
                        "INSERT OR IGNORE INTO crate_tracks(collection_id, track_id, added_at)
                         VALUES (?1, ?2, ?3)",
                        params![id, track, t],
                    )?;
                }
            }
            Some(CollectionKind::Playlist) => {
                let end: i64 = tx.query_row(
                    "SELECT coalesce(max(position) + 1, 0) FROM playlist_entries
                     WHERE collection_id = ?1",
                    [id],
                    |r| r.get(0),
                )?;
                for (i, track) in tracks.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO playlist_entries(collection_id, position, track_id)
                         VALUES (?1, ?2, ?3)",
                        params![id, end + i as i64, track],
                    )?;
                }
            }
            Some(CollectionKind::Flow) => {
                let mut graph = read::flow_graph(&tx, id)?;
                for &track in tracks {
                    graph.add_root(track, FLOW_SPACING);
                }
                write_graph(&tx, id, &graph)?;
            }
            Some(_) => return Err(StoreError::Refused("tracks can't be added to that")),
            None => return Err(StoreError::NoCollection(id)),
        }
        tx.execute(
            "UPDATE collections SET updated_at = ?2 WHERE id = ?1",
            params![id, t],
        )?;
        tx.commit()?;
        self.emit_collections()
    }

    /// Take tracks out of a crate, playlist (every place they're at, closing the
    /// gaps) or Flow (every card of theirs; what followed a card follows what it
    /// followed).
    pub fn remove_tracks(&mut self, id: i64, tracks: &[i64]) -> Result<()> {
        let tx = self.conn.transaction()?;
        match read::kind(&tx, id)? {
            Some(CollectionKind::Crate) => {
                for track in tracks {
                    tx.execute(
                        "DELETE FROM crate_tracks WHERE collection_id = ?1 AND track_id = ?2",
                        params![id, track],
                    )?;
                }
            }
            Some(CollectionKind::Playlist) => {
                let kept: Vec<i64> = read::collection_tracks(&tx, id)?
                    .into_iter()
                    .filter(|t| !tracks.contains(t))
                    .collect();
                tx.execute(
                    "DELETE FROM playlist_entries WHERE collection_id = ?1",
                    [id],
                )?;
                for (i, track) in kept.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO playlist_entries(collection_id, position, track_id)
                         VALUES (?1, ?2, ?3)",
                        params![id, i as i64, track],
                    )?;
                }
            }
            Some(CollectionKind::Flow) => {
                let mut graph = read::flow_graph(&tx, id)?;
                let gone: Vec<NodeId> = graph
                    .nodes
                    .iter()
                    .filter(|n| tracks.contains(&n.track))
                    .map(|n| n.id)
                    .collect();
                for n in gone {
                    graph.remove(n);
                }
                write_graph(&tx, id, &graph)?;
            }
            Some(_) => return Err(StoreError::Refused("tracks can't be removed from that")),
            None => return Err(StoreError::NoCollection(id)),
        }
        tx.commit()?;
        self.emit_collections()
    }

    /// Where a Flow's canvas is looking. Not an edit: the Flow's `updated_at` stays.
    pub fn save_flow_camera(&mut self, id: i64, camera: FlowCamera) -> Result<()> {
        let json = serde_json::to_string(&camera).expect("four numbers serialise");
        let n = self.conn.execute(
            "UPDATE collections SET view_state = ?2 WHERE id = ?1 AND kind = 'flow'",
            params![id, json],
        )?;
        if n == 0 {
            return Err(StoreError::NoCollection(id));
        }
        Ok(())
    }

    /// A smart playlist's query. Refused if it doesn't parse.
    pub fn set_query(&mut self, id: i64, q: &str) -> Result<()> {
        query::parse(q).map_err(|e| StoreError::BadQuery(e.to_string()))?;
        let n = self.conn.execute(
            "UPDATE collections SET query = ?2, updated_at = ?3 WHERE id = ?1 AND kind = 'smart'",
            params![id, q, now()],
        )?;
        if n == 0 {
            return Err(StoreError::NoCollection(id));
        }
        self.emit_collections()
    }
}

/// Where the Store puts cards it adds to a Flow (the canvas's card size plus gaps).
const FLOW_SPACING: Spacing = Spacing {
    col: 326.0,
    row: 126.0,
};

/// A cue label as typed: trimmed, and nothing left is no label.
fn tidy(label: Option<&str>) -> Option<&str> {
    label.map(str::trim).filter(|s| !s.is_empty())
}

/// Make the stored Flow match `graph`, writing only what changed.
fn write_graph(tx: &Connection, id: i64, graph: &FlowGraph) -> Result<()> {
    let old = read::flow_graph(tx, id)?;
    let new_edges: HashSet<(NodeId, NodeId)> = graph.edges.iter().copied().collect();
    for &(a, b) in &old.edges {
        if !new_edges.contains(&(a, b)) {
            tx.execute(
                "DELETE FROM flow_edges WHERE collection_id = ?1 AND from_node = ?2 AND to_node = ?3",
                params![id, a as i64, b as i64],
            )?;
        }
    }
    let ids: HashSet<NodeId> = graph.nodes.iter().map(|n| n.id).collect();
    for n in old.nodes.iter().filter(|n| !ids.contains(&n.id)) {
        tx.execute(
            "DELETE FROM flow_nodes WHERE collection_id = ?1 AND id = ?2",
            params![id, n.id as i64],
        )?;
    }
    let old_nodes: HashMap<NodeId, &FlowNode> = old.nodes.iter().map(|n| (n.id, n)).collect();
    for n in &graph.nodes {
        if old_nodes.get(&n.id) != Some(&n) {
            tx.execute(
                "INSERT INTO flow_nodes(collection_id, id, track_id, x, y)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(collection_id, id) DO UPDATE SET
                   track_id = excluded.track_id, x = excluded.x, y = excluded.y",
                params![id, n.id as i64, n.track, n.x as f64, n.y as f64],
            )?;
        }
    }
    let old_edges: HashSet<(NodeId, NodeId)> = old.edges.into_iter().collect();
    for &(a, b) in new_edges.difference(&old_edges) {
        tx.execute(
            "INSERT INTO flow_edges(collection_id, from_node, to_node) VALUES (?1, ?2, ?3)",
            params![id, a as i64, b as i64],
        )?;
    }
    tx.execute(
        "UPDATE collections SET updated_at = ?2 WHERE id = ?1",
        params![id, now()],
    )?;
    Ok(())
}

// ---- helpers (inside a transaction) ---------------------------------------------------

pub(super) fn save_features(
    c: &Connection,
    track: i64,
    features: &[(&'static str, f64)],
    run: Option<i64>,
) -> Result<()> {
    let mut st = c.prepare_cached(
        "INSERT INTO track_features(track_id, name, value, run_id) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(track_id, name) DO UPDATE SET value = excluded.value, run_id = excluded.run_id",
    )?;
    for (name, value) in features {
        if value.is_finite() {
            st.execute(params![track, name, value, run])?;
        }
    }
    Ok(())
}

/// Save a waveform and its thumbnail; returns the thumbnail.
fn put_waveform(c: &Connection, track: i64, data: &[u8]) -> Result<Vec<u8>> {
    let preview = earshot_engine::waveform::preview(data);
    let mut st = c.prepare_cached(
        "INSERT INTO blobs(track_id, kind, format_version, params, data) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(track_id, kind) DO UPDATE SET format_version = excluded.format_version,
           params = excluded.params, data = excluded.data",
    )?;
    let params_ = waveform_params(earshot_engine::waveform::BINS_PER_SECOND);
    st.execute(params![track, "waveform", WAVEFORM_VERSION, params_, data])?;
    st.execute(params![
        track,
        "waveform_preview",
        WAVEFORM_VERSION,
        None::<String>,
        preview
    ])?;
    Ok(preview)
}

/// A grid's BPM, as the track list shows it: a steady grid's own tempo, exactly; the
/// average of a markers grid's beats, to 0.01.
pub fn grid_bpm(grid: &Beatgrid) -> f64 {
    match grid.as_constant() {
        Some(c) => c.bpm,
        None => (grid.mean_bpm() * 100.0).round() / 100.0,
    }
}

/// Save a grid's model: a steady grid's numbers, or a markers grid's beats.
fn insert_grid(
    c: &Connection,
    track: i64,
    grid: &Beatgrid,
    source: GridSource,
    run: Option<i64>,
) -> Result<i64> {
    let bpm = grid_bpm(grid);
    let (kind, anchor, anchor_pos, meter, beats) = match grid.as_constant() {
        Some(k) => (
            "constant",
            Some(k.anchor),
            Some(k.anchor_pos),
            Some(k.meter),
            None,
        ),
        None => ("markers", None, None, None, Some(grid.to_bytes())),
    };
    c.execute(
        "INSERT INTO beatgrids(track_id, source, run_id, kind, bpm, anchor_s, anchor_pos,
                               meter, beats, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            track,
            source.as_str(),
            run,
            kind,
            bpm,
            anchor,
            anchor_pos,
            meter,
            beats,
            now()
        ],
    )?;
    Ok(c.last_insert_rowid())
}

/// Record an analyser's result, with the hash of the audio it ran on: the track's
/// present file, the one `read::track_path` gives the analysis to decode.
fn put_result(
    c: &Connection,
    track: i64,
    analyser: &str,
    version: u32,
    run: Option<i64>,
    error: Option<&str>,
) -> Result<()> {
    c.prepare_cached(
        "INSERT INTO analysis_results(track_id, analyser, version, content_hash, run_id, error, analysed_at)
         VALUES (?1, ?2, ?3,
                 (SELECT content_hash FROM files WHERE track_id = ?1 AND missing_since IS NULL
                  ORDER BY id LIMIT 1),
                 ?4, ?5, ?6)
         ON CONFLICT(track_id, analyser) DO UPDATE SET version = excluded.version,
           content_hash = excluded.content_hash, run_id = excluded.run_id,
           error = excluded.error, analysed_at = excluded.analysed_at",
    )?
    .execute(params![track, analyser, version, run, error, now()])?;
    Ok(())
}

/// Make a grid the active one; the track's BPM always comes with it.
fn activate_grid(c: &Connection, track: i64, grid: i64, bpm: f64) -> Result<()> {
    c.execute(
        "UPDATE tracks SET active_grid_id = ?2, bpm = ?3 WHERE id = ?1",
        params![track, grid, bpm],
    )?;
    Ok(())
}

fn effective(c: &Connection, track: i64) -> Result<(Option<f64>, Option<GridSource>)> {
    let bpm: Option<f64> = c
        .query_row("SELECT bpm FROM tracks WHERE id = ?1", [track], |r| {
            r.get(0)
        })
        .optional()?
        .flatten();
    Ok((bpm, read::active_grid(c, track)?.map(|g| g.1)))
}

fn put_meta(
    c: &Connection,
    track: i64,
    field: &str,
    source: &str,
    value: Option<&str>,
    t: i64,
) -> Result<()> {
    match value {
        Some(v) => c
            .prepare_cached(
                "INSERT INTO track_meta(track_id, field, source, value, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(track_id, field, source) DO UPDATE SET
                   value = excluded.value, updated_at = excluded.updated_at
                 WHERE value != excluded.value",
            )?
            .execute(params![track, field, source, v, t])?,
        None => c
            .prepare_cached(
                "DELETE FROM track_meta WHERE track_id = ?1 AND field = ?2 AND source = ?3",
            )?
            .execute(params![track, field, source])?,
    };
    if field == "key" {
        refresh_key(c, track)?;
    }
    Ok(())
}

/// Copy the track's key (the resolved `key` field, if it reads as one) into
/// `tracks.key_pc` and `key_minor`, so queries can test it without parsing text.
fn refresh_key(c: &Connection, track: i64) -> Result<()> {
    let text: Option<String> = c
        .prepare_cached(
            "SELECT value FROM track_meta_resolved WHERE track_id = ?1 AND field = 'key'",
        )?
        .query_row([track], |r| r.get(0))
        .optional()?;
    let key = text.as_deref().and_then(parse_key);
    c.prepare_cached("UPDATE tracks SET key_pc = ?2, key_minor = ?3 WHERE id = ?1")?
        .execute(params![
            track,
            key.map(pitch_class),
            key.map(|k| k.minor as i64)
        ])?;
    Ok(())
}

/// Fill `key_pc` / `key_minor` for tracks that have a key but not those columns (a
/// database from before they were kept). Run when the database is opened.
pub(super) fn fill_keys(c: &Connection) -> Result<()> {
    let ids: Vec<i64> = c
        .prepare(
            "SELECT t.id FROM tracks t WHERE t.key_pc IS NULL AND EXISTS
               (SELECT 1 FROM track_meta m WHERE m.track_id = t.id AND m.field = 'key')",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in ids {
        refresh_key(c, id)?;
    }
    Ok(())
}

/// What the file's tags say, as source 'tag'. Never touches other sources.
fn write_tag_meta(c: &Connection, track: i64, tags: &TrackTags, t: i64) -> Result<()> {
    let bpm = tags.bpm.map(|b| format!("{b}"));
    for (field, value) in [
        ("title", tags.title.as_deref()),
        ("artist", tags.artist.as_deref()),
        ("bpm", bpm.as_deref()),
        ("key", tags.key.as_deref()),
    ] {
        put_meta(c, track, field, "tag", value.filter(|v| !v.is_empty()), t)?;
    }
    reindex(c, track)
}

const SEARCH_FIELDS: [&str; 5] = ["title", "artist", "album", "genre", "comment"];

/// Refresh a track's row in the full-text index from its resolved metadata.
fn reindex(c: &Connection, track: i64) -> Result<()> {
    let mut best: HashMap<String, (u8, String)> = HashMap::new();
    {
        let mut st = c.prepare_cached(
            "SELECT field, source, value FROM track_meta WHERE track_id = ?1
             AND field IN ('title', 'artist', 'album', 'genre', 'comment')",
        )?;
        let mut q = st.query([track])?;
        while let Some(r) = q.next()? {
            let (field, source, value): (String, String, String) =
                (r.get(0)?, r.get(1)?, r.get(2)?);
            let rank = source_rank(&source);
            if best.get(&field).is_none_or(|b| rank < b.0) {
                best.insert(field, (rank, value));
            }
        }
    }
    c.prepare_cached("DELETE FROM track_search WHERE rowid = ?1")?
        .execute([track])?;
    let v: Vec<Option<String>> = SEARCH_FIELDS
        .iter()
        .map(|f| best.get(*f).map(|b| b.1.clone()))
        .collect();
    c.prepare_cached(
        "INSERT INTO track_search(rowid, title, artist, album, genre, comment)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![track, v[0], v[1], v[2], v[3], v[4]])?;
    Ok(())
}

/// A track (other than `except`) with this audio whose files are all missing: where
/// a moved file came from.
fn orphan_with_hash(c: &Connection, hash: &Hash, except: Option<i64>) -> Result<Option<i64>> {
    Ok(c.query_row(
        "SELECT f.track_id FROM files f JOIN tracks t ON t.id = f.track_id
         WHERE f.content_hash = ?1 AND t.deleted_at IS NULL
           AND (?2 IS NULL OR f.track_id != ?2)
           AND NOT EXISTS (SELECT 1 FROM files g
                           WHERE g.track_id = f.track_id AND g.missing_since IS NULL)
         ORDER BY f.track_id LIMIT 1",
        params![hash.to_vec(), except],
        |r| r.get(0),
    )
    .optional()?)
}

/// Fold track `from` into `into`: its files, and wherever it's used, move over, then
/// it's deleted. `into` keeps its own analysis, user metadata and cues; it takes
/// `from`'s tags (they're the present file's) and, if it had no grid, `from`'s
/// analysis.
fn merge(c: &Connection, from: i64, into: i64) -> Result<()> {
    let p = params![from, into];
    c.execute("UPDATE files SET track_id = ?2 WHERE track_id = ?1", p)?;
    c.execute("UPDATE flow_nodes SET track_id = ?2 WHERE track_id = ?1", p)?;
    c.execute(
        "UPDATE playlist_entries SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute(
        "UPDATE OR IGNORE crate_tracks SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute(
        "UPDATE OR IGNORE track_tags SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute(
        "UPDATE OR IGNORE cues SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute("UPDATE plays SET track_id = ?2 WHERE track_id = ?1", p)?;
    c.execute(
        "UPDATE OR IGNORE external_links SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute(
        "DELETE FROM track_meta WHERE track_id = ?2 AND source = 'tag'",
        p,
    )?;
    c.execute(
        "UPDATE OR IGNORE track_meta SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    let has_grid: bool = c.query_row(
        "SELECT active_grid_id IS NOT NULL FROM tracks WHERE id = ?1",
        [into],
        |r| r.get(0),
    )?;
    if !has_grid {
        c.execute(
            "UPDATE analysis_runs SET track_id = ?2 WHERE track_id = ?1",
            p,
        )?;
        c.execute("UPDATE beatgrids SET track_id = ?2 WHERE track_id = ?1", p)?;
        c.execute(
            "UPDATE tracks SET (active_grid_id, bpm) =
               (SELECT active_grid_id, bpm FROM tracks WHERE id = ?1) WHERE id = ?2",
            p,
        )?;
    }
    c.execute(
        "UPDATE OR IGNORE track_features SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute(
        "UPDATE OR IGNORE analysis_results SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute(
        "UPDATE OR IGNORE blobs SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute(
        "UPDATE OR IGNORE embeddings SET track_id = ?2 WHERE track_id = ?1",
        p,
    )?;
    c.execute(
        "UPDATE tracks SET duration_s = coalesce((SELECT duration_s FROM tracks WHERE id = ?1), duration_s)
         WHERE id = ?2",
        p,
    )?;
    c.execute("DELETE FROM track_search WHERE rowid = ?1", [from])?;
    c.execute("DELETE FROM tracks WHERE id = ?1", [from])?;
    refresh_key(c, into)?;
    reindex(c, into)
}
