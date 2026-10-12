use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use earshot_engine::beatgrid::{Beatgrid, Constant, GridModel};
use earshot_engine::decode::TrackTags;

use super::*;
use crate::core::collections::CollectionKind::{Crate, Flow, Folder, Playlist, Smart};
use crate::core::collections::{FlowGraph, FlowNode};
use write::save_features;

/// A fresh database in its own folder, which also serves as the library root.
fn setup(name: &str) -> (PathBuf, Writer) {
    let dir = std::env::temp_dir().join(format!("earshot-store-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    let w = Writer::new(open_writer(&dir.join(FILE_NAME)).unwrap());
    (dir, w)
}

fn tags(title: &str, artist: &str) -> TrackTags {
    TrackTags {
        title: Some(title.into()),
        artist: Some(artist.into()),
        bpm: Some(124.0),
        key: Some("8A".into()),
        duration: Some(300.0),
    }
}

fn file(path: &Path, title: &str, hash: Option<u8>) -> ScannedFile {
    ScannedFile {
        path: path.into(),
        size: 1000,
        mtime_ns: 1,
        tags: tags(title, "Someone"),
        tag_hash: hash.map(|h| [h; 32]),
    }
}

/// Scan these files (all of them changed) under `dir/music`.
fn scan(w: &mut Writer, dir: &Path, files: &[ScannedFile]) {
    let root = dir.join("music");
    let found: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
    w.apply_scan(&[root], &found, files).unwrap();
}

fn library(w: &mut Writer) -> Vec<LibTrack> {
    read::tracks(w.conn(), None).unwrap()
}

fn grid(bpm: f64) -> Beatgrid {
    Beatgrid::constant(bpm, 0.0, 60.0).unwrap()
}

#[test]
fn an_old_development_database_is_refused() {
    let (dir, w) = setup("older");
    w.conn().pragma_update(None, "user_version", 2).unwrap();
    drop(w);
    let err = open_writer(&dir.join(FILE_NAME)).unwrap_err();
    assert!(matches!(err, StoreError::TooOld { found: 2 }));
    assert!(err.to_string().contains("delete library.db"), "{err}");
}

/// A steady grid is saved as its numbers (no beat list), a markers grid as its beats,
/// and both come back exactly. The track's BPM is a steady grid's own.
#[test]
fn grids_are_saved_as_their_models() {
    let (_dir, mut w, id) = one_track("models");
    let c = Constant {
        bpm: 128.37,
        anchor: 12.3456789,
        anchor_pos: 3,
        meter: 4,
    };
    let steady = Beatgrid::from_model(GridModel::Constant(c), 300.0).unwrap();
    let g = w.save_user_grid(id, &steady).unwrap();
    let row: (String, f64, Option<f64>, Option<Vec<u8>>) = w
        .conn()
        .query_row(
            "SELECT kind, bpm, anchor_s, beats FROM beatgrids WHERE id = ?1",
            [g],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(row, ("constant".into(), 128.37, Some(12.3456789), None));
    assert_eq!(library(&mut w)[0].bpm, Some(128.37));
    let (model, src) = read::grid(w.conn(), id).unwrap().unwrap();
    assert_eq!((model, src), (GridModel::Constant(c), GridSource::User));

    let markers = Beatgrid::new(vec![0.25, 0.75, 1.27, 1.76, 2.3], vec![4, 1, 2, 3, 4]).unwrap();
    w.save_user_grid(id, &markers).unwrap();
    let (model, _) = read::grid(w.conn(), id).unwrap().unwrap();
    assert_eq!(model, markers.model());
    assert_eq!(Beatgrid::from_model(model, 3.0).unwrap(), markers);
    assert_eq!(library(&mut w)[0].bpm, Some(117.07), "the average, to 0.01");

    // A row with the wrong columns for its kind can't be saved.
    assert!(
        w.conn()
            .execute(
                "INSERT INTO beatgrids(track_id, source, kind, bpm, beats, created_at)
                 VALUES (?1, 'user', 'constant', 120, x'00', 0)",
                [id],
            )
            .is_err()
    );
}

#[test]
fn opens_twice_and_is_strict() {
    let (dir, w) = setup("open");
    drop(w);
    let w = Writer::new(open_writer(&dir.join(FILE_NAME)).unwrap());
    let c = w.conn();
    let mode: String = c
        .pragma_query_value(None, "journal_mode", |r| r.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let sync: i64 = c
        .pragma_query_value(None, "synchronous", |r| r.get(0))
        .unwrap();
    assert_eq!(sync, 1, "NORMAL");
    // STRICT: a string in an INTEGER column is refused rather than stored.
    assert!(
        c.execute("INSERT INTO tracks(added_at) VALUES ('yesterday')", [])
            .is_err()
    );
}

#[test]
fn a_newer_database_is_refused() {
    let (dir, w) = setup("newer");
    w.conn().pragma_update(None, "user_version", 99).unwrap();
    drop(w);
    assert!(matches!(
        open_writer(&dir.join(FILE_NAME)),
        Err(StoreError::TooNew { found: 99, .. })
    ));
}

#[test]
fn rescans_keep_what_the_user_typed() {
    let (dir, mut w) = setup("meta");
    w.add_roots(&[dir.join("music")]).unwrap();
    let a = dir.join("music/a.mp3");
    scan(&mut w, &dir, &[file(&a, "Tag Title", None)]);
    let id = library(&mut w)[0].id;
    w.set_meta(id, "title", "user", Some("My Title")).unwrap();
    assert_eq!(library(&mut w)[0].title, "My Title");

    // The file is retagged: the tag value changes underneath, the user's stays on top.
    scan(&mut w, &dir, &[file(&a, "New Tag Title", None)]);
    let t = &library(&mut w)[0];
    assert_eq!((t.id, t.title.as_str()), (id, "My Title"));
    let tag: String = w
        .conn()
        .query_row(
            "SELECT value FROM track_meta WHERE track_id = ?1 AND field = 'title' AND source = 'tag'",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tag, "New Tag Title");

    // Clearing the user's value shows the tag's again; search follows.
    w.set_meta(id, "title", "user", None).unwrap();
    assert_eq!(library(&mut w)[0].title, "New Tag Title");
    assert_eq!(read::search(w.conn(), "new").unwrap(), vec![id]);
    assert!(read::search(w.conn(), "my").unwrap().is_empty());
}

#[test]
fn a_track_with_no_title_shows_its_file_name() {
    let (dir, mut w) = setup("stem");
    w.add_roots(&[dir.join("music")]).unwrap();
    let mut f = file(&dir.join("music/Some Song.mp3"), "", None);
    f.tags.title = None;
    scan(&mut w, &dir, &[f]);
    assert_eq!(library(&mut w)[0].title, "Some Song");
}

#[test]
fn files_outside_the_roots_are_kept_but_not_listed() {
    let (dir, mut w) = setup("roots");
    w.add_roots(&[dir.join("music")]).unwrap();
    scan(&mut w, &dir, &[file(&dir.join("music/a.mp3"), "A", None)]);
    assert_eq!(library(&mut w).len(), 1);
    w.remove_root(&dir.join("music").to_string_lossy()).unwrap();
    assert!(library(&mut w).is_empty());
    let n: i64 = w
        .conn()
        .query_row("SELECT count(*) FROM tracks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

/// A track with an analysis, a hand-fixed grid and a place in a Flow.
fn curated_track(w: &mut Writer, dir: &Path, path: &Path, hash: Option<u8>) -> i64 {
    w.add_roots(&[dir.join("music")]).unwrap();
    scan(w, dir, &[file(path, "Moved", hash)]);
    let id = library(w)[0].id;
    w.save_user_grid(id, &grid(128.0)).unwrap();
    w.create_collection(500, None, Flow, "Set", None).unwrap();
    w.save_flow(500, &graph(&[(1, id)], &[])).unwrap();
    id
}

/// A Flow of (card, track) with these links.
fn graph(nodes: &[(u64, i64)], edges: &[(u64, u64)]) -> FlowGraph {
    FlowGraph::new(
        nodes
            .iter()
            .map(|&(id, track)| FlowNode {
                id,
                track,
                x: id as f32,
                y: 0.0,
            })
            .collect(),
        edges.to_vec(),
    )
}

#[test]
fn a_moved_file_with_a_stamped_hash_keeps_its_track() {
    let (dir, mut w) = setup("move-tag");
    let old = dir.join("music/old/a.mp3");
    let id = curated_track(&mut w, &dir, &old, Some(7));
    // Next scan: the file is somewhere else, with the same TXXX:AUDIO_SHA256.
    scan(
        &mut w,
        &dir,
        &[file(&dir.join("music/new/a.mp3"), "Moved", Some(7))],
    );
    let lib = library(&mut w);
    assert_eq!(lib.len(), 1);
    assert_eq!(lib[0].id, id);
    assert!(lib[0].path.ends_with("new/a.mp3"));
    assert_eq!(lib[0].bpm, Some(128.0), "kept its grid");
    let missing: Option<i64> = w
        .conn()
        .query_row(
            "SELECT missing_since FROM files WHERE path = ?1",
            [path_bytes(&old)],
            |r| r.get(0),
        )
        .unwrap();
    assert!(missing.is_some(), "the old location is marked missing");
    assert_eq!(
        read::flow(w.conn(), 500).unwrap().unwrap().2.nodes[0].track,
        id
    );
}

#[test]
fn a_moved_file_hashed_later_is_merged_back() {
    let (dir, mut w) = setup("move-hash");
    let id = curated_track(&mut w, &dir, &dir.join("music/a.mp3"), None);
    let old_file: i64 = w
        .conn()
        .query_row("SELECT id FROM files", [], |r| r.get(0))
        .unwrap();
    assert_eq!(w.set_content_hash(old_file, &[9; 32]).unwrap(), None);
    // Moved, no stamped hash: at first it looks like a new track.
    let new_path = dir.join("music/renamed.mp3");
    scan(&mut w, &dir, &[file(&new_path, "Moved", None)]);
    let lib = library(&mut w);
    assert_eq!(lib.len(), 1);
    let newcomer = lib[0].id;
    assert_ne!(newcomer, id);
    // The background hash finds it's the same audio.
    let (file_id, _) = read::files_without_hash(w.conn()).unwrap()[0].clone();
    assert_eq!(
        w.set_content_hash(file_id, &[9; 32]).unwrap(),
        Some((newcomer, id))
    );
    let lib = library(&mut w);
    assert_eq!(lib.len(), 1);
    assert_eq!((lib[0].id, lib[0].bpm), (id, Some(128.0)));
    assert_eq!(read::track_path(w.conn(), id).unwrap(), new_path);
    assert_eq!(
        read::flow(w.conn(), 500).unwrap().unwrap().2.nodes[0].track,
        id
    );
    assert!(
        w.take_events()
            .iter()
            .any(|e| matches!(e, StoreEvent::Library(l) if l.len() == 1))
    );
}

#[test]
fn copies_of_the_same_audio_are_duplicates() {
    let (dir, mut w) = setup("dupes");
    w.add_roots(&[dir.join("music")]).unwrap();
    scan(
        &mut w,
        &dir,
        &[
            file(&dir.join("music/a.mp3"), "A", Some(1)),
            file(&dir.join("music/copy of a.mp3"), "A", Some(1)),
            file(&dir.join("music/b.mp3"), "B", Some(2)),
        ],
    );
    let lib = library(&mut w);
    assert_eq!(
        lib.len(),
        3,
        "a copy whose original is present is its own track"
    );
    let dupes = read::duplicates(w.conn()).unwrap();
    assert_eq!(dupes.len(), 1);
    assert_eq!(dupes[0].len(), 2);
}

fn analysis(track: i64, bpm: f64, expected: Option<i64>, force: bool) -> AnalysisSave {
    AnalysisSave {
        track,
        analyser: "static".into(),
        mode: "static",
        started_at: 0,
        error: None,
        features: vec![("energy", 0.5), ("drifts", 1.0)],
        waveform: Some(vec![10; 4 * 150]),
        grid: Some(GridSave {
            grid: grid(bpm),
            source: GridSource::Analysis,
            expected_active: expected,
            force,
        }),
        results: vec![("waveform", 1, None), ("grid", 2, None)],
    }
}

fn one_track(name: &str) -> (PathBuf, Writer, i64) {
    let (dir, mut w) = setup(name);
    w.add_roots(&[dir.join("music")]).unwrap();
    scan(&mut w, &dir, &[file(&dir.join("music/a.mp3"), "A", None)]);
    let id = library(&mut w)[0].id;
    (dir, w, id)
}

#[test]
fn the_first_beat_is_kept_with_the_track() {
    let (dir, mut w, id) = one_track("first-beat");
    // Not analysed yet, or by a grid analyser from before first_beat.
    w.save_analysis(analysis(id, 125.0, None, false)).unwrap();
    assert_eq!(read::first_beat(w.conn(), id).unwrap(), None);
    let mut s = analysis(id, 125.0, None, false);
    s.features.push(("first_beat", 12.3));
    w.save_analysis(s).unwrap();
    drop(w);
    // Read back after a restart, with no analysis.
    let c = open_writer(&dir.join(FILE_NAME)).unwrap();
    assert_eq!(read::first_beat(&c, id).unwrap(), Some(12.3));
}

#[test]
fn stale_analyser_results_are_one_query() {
    let (dir, mut w) = setup("stale");
    w.add_roots(&[dir.join("music")]).unwrap();
    scan(
        &mut w,
        &dir,
        &[
            file(&dir.join("music/a.mp3"), "A", None),
            file(&dir.join("music/b.mp3"), "B", None),
        ],
    );
    let ids: Vec<i64> = library(&mut w).iter().map(|t| t.id).collect();
    let (a, b) = (ids[0], ids[1]);
    let stale = |w: &Writer, analyser: &str, v: u32| read::stale_tracks(w.conn(), analyser, v).unwrap();
    w.save_analysis(analysis(a, 125.0, None, false)).unwrap();
    // Only the track never analysed…
    assert_eq!(stale(&w, "grid", 2), [b]);
    assert_eq!(stale(&w, "waveform", 1), [b]);
    // …until the analyser's version goes up.
    assert_eq!(stale(&w, "grid", 3), [a, b]);
    // Its file's hash, worked out later, is taken to be the audio it analysed.
    let file: i64 = w
        .conn()
        .query_row("SELECT id FROM files WHERE track_id = ?1", [a], |r| r.get(0))
        .unwrap();
    w.set_content_hash(file, &[7; 32]).unwrap();
    assert_eq!(stale(&w, "grid", 2), [b]);
    // Different audio under the same name (re-encoded, say): stale again.
    w.conn()
        .execute("UPDATE files SET content_hash = ?1 WHERE id = ?2", rusqlite::params![vec![8u8; 32], file])
        .unwrap();
    assert_eq!(stale(&w, "grid", 2), [a, b]);
    let r = read::analysis_results(w.conn(), a).unwrap();
    assert!(r.iter().all(|r| !r.same_audio && !r.failed));

    // Loudness measured on a deck load counts; a failed run doesn't.
    w.save_measured(b, &[("lufs", -9.0), ("peak", 0.9), ("energy", 0.4)])
        .unwrap();
    assert_eq!((stale(&w, "loudness", 1), stale(&w, "energy", 1)), (vec![a], vec![a]));
    let mut failed = analysis(b, 125.0, None, false);
    (failed.grid, failed.results) = (None, vec![("grid", 2, Some("no beats".into()))]);
    w.save_analysis(failed).unwrap();
    assert!(stale(&w, "grid", 2).contains(&b));
}

#[test]
fn the_analysis_queue_and_settings_are_saved() {
    let (_dir, mut w, id) = one_track("jobs");
    let job = |analyser: &str, want: &str| JobRow {
        track: id,
        analyser: analyser.into(),
        want: want.into(),
        params: Some("dynamic".into()),
    };
    w.save_jobs(id, &[job("waveform", "missing"), job("grid", "force")])
        .unwrap();
    assert_eq!(
        read::queued_jobs(w.conn()).unwrap(),
        [job("waveform", "missing"), job("grid", "force")]
    );
    // Saving a track's jobs replaces them; none left clears them.
    w.save_jobs(id, &[job("grid", "stale")]).unwrap();
    assert_eq!(read::queued_jobs(w.conn()).unwrap(), [job("grid", "stale")]);
    w.save_jobs(id, &[]).unwrap();
    assert!(read::queued_jobs(w.conn()).unwrap().is_empty());
    // A track deleted meanwhile is skipped, not an error.
    w.save_jobs(9999, &[JobRow { track: 9999, ..job("grid", "stale") }])
        .unwrap();
    assert!(read::queued_jobs(w.conn()).unwrap().is_empty());

    assert_eq!(read::setting(w.conn(), "analysis.background_workers").unwrap(), None);
    w.apply(StoreCmd::SetSetting {
        key: "analysis.background_workers".into(),
        value: "2".into(),
    })
    .unwrap();
    assert_eq!(
        read::setting(w.conn(), "analysis.background_workers").unwrap().as_deref(),
        Some("2")
    );
}

/// Bug 1: a grid fixed by hand while analysis ran isn't overwritten.
/// Bug 4: and analysis doesn't change the BPM that came with it.
#[test]
fn analysis_keeps_a_grid_fixed_while_it_ran() {
    let (_dir, mut w, id) = one_track("cas");
    // The job starts: no grid yet (expected_active None). Meanwhile the user fixes it.
    let user = w.save_user_grid(id, &grid(126.0)).unwrap();
    let out = w.save_analysis(analysis(id, 125.0, None, false)).unwrap();
    assert!(!out.switched);
    assert_eq!((out.bpm, out.source), (Some(126.0), Some(GridSource::User)));
    let t = &library(&mut w)[0];
    assert_eq!(
        (t.bpm, t.grid_source),
        (Some(126.0), Some(GridSource::User))
    );
    assert!(t.drifts, "the rest of the analysis is still saved");
    assert_eq!(read::active_grid(w.conn(), id).unwrap().unwrap().0, user);

    // Even a forced re-analysis loses if the grid changed after it started…
    let out = w.save_analysis(analysis(id, 125.0, None, true)).unwrap();
    assert!(!out.switched);
    // …but wins when the user asked for it and nothing changed meanwhile. The hand
    // grid is still there to go back to.
    let out = w
        .save_analysis(analysis(id, 125.0, Some(user), true))
        .unwrap();
    assert!(out.switched);
    assert_eq!(out.bpm, Some(125.0));
    let grids: i64 = w
        .conn()
        .query_row(
            "SELECT count(*) FROM beatgrids WHERE track_id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(grids, 4);
}

#[test]
fn analysis_of_a_new_track_sets_grid_bpm_and_thumbnail() {
    let (_dir, mut w, id) = one_track("analyse");
    w.take_events();
    let out = w.save_analysis(analysis(id, 125.0, None, false)).unwrap();
    assert!(out.switched);
    let events = w.take_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StoreEvent::Previews(p) if p[0].0 == id))
    );
    assert!(events.iter().any(
        |e| matches!(e, StoreEvent::TracksChanged(t) if t[0].bpm == Some(125.0) && t[0].energy == Some(0.5))
    ));
    let (g, src) = read::grid(w.conn(), id).unwrap().unwrap();
    assert_eq!((g, src), (grid(125.0).model(), GridSource::Analysis));
    assert!(
        read::waveform(w.conn(), id, earshot_engine::waveform::BINS_PER_SECOND)
            .unwrap()
            .is_some()
    );
}

/// Bug 4: a grid edit changes the track's BPM, and the library hears about it.
#[test]
fn a_grid_edit_updates_the_bpm() {
    let (_dir, mut w, id) = one_track("edit");
    w.save_analysis(analysis(id, 125.0, None, false)).unwrap();
    w.take_events();
    w.save_user_grid(id, &grid(130.0)).unwrap();
    let events = w.take_events();
    assert!(events.iter().any(|e| matches!(
        e,
        StoreEvent::TracksChanged(t) if t[0].bpm == Some(130.0) && t[0].grid_source == Some(GridSource::User)
    )));
}

#[test]
fn hot_cues_and_memories_save_and_load() {
    let (_dir, mut w, id) = one_track("cues");
    let mut hot = [None; earshot_engine::HOTCUES];
    hot[0] = Some(1.5);
    hot[3] = Some(30.25);
    w.save_hotcues(id, &hot).unwrap();
    assert_eq!(read::hotcues(w.conn(), id).unwrap(), hot);
    // Saving again replaces them.
    hot[0] = None;
    hot[7] = Some(90.0);
    w.save_hotcues(id, &hot).unwrap();
    assert_eq!(read::hotcues(w.conn(), id).unwrap(), hot);

    w.take_events();
    w.add_memory(id, 40.0, Some(48.0)).unwrap();
    w.add_memory(id, 10.0, None).unwrap();
    w.add_memory(id, 10.001, None).unwrap(); // the same cue again: not saved twice
    let m = read::memories(w.conn(), id).unwrap();
    assert_eq!(
        m.iter().map(|m| (m.start, m.end)).collect::<Vec<_>>(),
        [(10.0, None), (40.0, Some(48.0))]
    );
    assert!(w.take_events().iter().all(
        |e| matches!(e, StoreEvent::Cues { track, cues } if *track == id && !cues.memories.is_empty())
    ));
    let kinds: Vec<i64> = w
        .conn()
        .prepare("SELECT kind FROM cues WHERE track_id = ?1 ORDER BY kind")
        .unwrap()
        .query_map([id], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(kinds, [0, 0, 1, 2], "two hot cues, a memory cue and a loop");

    // Deleting a memory cue leaves the hot cues alone.
    w.delete_memory(id, m[0].id).unwrap();
    assert_eq!(read::memories(w.conn(), id).unwrap(), [m[1].clone()]);
    assert_eq!(read::hotcues(w.conn(), id).unwrap(), hot);
}

#[test]
fn hot_cue_labels_follow_their_pad() {
    let (_dir, mut w, id) = one_track("hot-labels");
    let mut hot = [None; earshot_engine::HOTCUES];
    hot[1] = Some(4.0);
    hot[2] = Some(8.0);
    w.save_hotcues(id, &hot).unwrap();
    w.set_hotcue_label(id, 1, Some("  Drop ")).unwrap();
    w.set_hotcue_label(id, 2, Some("Break")).unwrap();
    // A pad that isn't set has nothing to name.
    w.set_hotcue_label(id, 5, Some("Nope")).unwrap();
    let labels = |w: &Writer| read::hot_labels(w.conn(), id).unwrap();
    assert_eq!(labels(&w)[1].as_deref(), Some("Drop"), "trimmed");
    assert_eq!(labels(&w)[5], None);

    // Saving times (as the engine reports them) keeps the labels of pads still set,
    // even at a new time.
    w.take_events();
    hot[1] = Some(4.5);
    w.apply(StoreCmd::SaveHotcues {
        track: id,
        cues: hot,
    })
    .unwrap();
    assert_eq!(read::hotcues(w.conn(), id).unwrap(), hot);
    assert_eq!(labels(&w)[1].as_deref(), Some("Drop"));
    assert!(matches!(
        w.take_events().last(),
        Some(StoreEvent::Cues { track, cues }) if *track == id && cues.hot_labels[2].is_some()
    ));

    // Clearing a pad clears its label: set again, it comes back unnamed.
    hot[2] = None;
    w.save_hotcues(id, &hot).unwrap();
    hot[2] = Some(9.0);
    w.save_hotcues(id, &hot).unwrap();
    assert_eq!(labels(&w)[2], None);
    // And an empty name is no name.
    w.set_hotcue_label(id, 1, Some("")).unwrap();
    assert!(labels(&w).iter().all(Option::is_none));
}

#[test]
fn memory_labels_and_colours_round_trip() {
    let (_dir, mut w, id) = one_track("memory-labels");
    w.add_memory(id, 10.0, None).unwrap();
    w.add_memory(id, 20.0, Some(24.0)).unwrap();
    let m = read::memories(w.conn(), id).unwrap();
    assert_eq!((m[0].label.as_deref(), m[0].color), (None, None));

    w.take_events();
    w.set_memory_label(id, m[0].id, Some("Vocal in")).unwrap();
    w.set_memory_color(id, m[1].id, Some(0x30_5a_ff)).unwrap();
    let after = read::memories(w.conn(), id).unwrap();
    assert_eq!(after[0].label.as_deref(), Some("Vocal in"));
    assert_eq!(after[1].color, Some(0x30_5a_ff));
    // Each edit is answered with the track's cues as they now are.
    let events = w.take_events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events.last(),
        Some(StoreEvent::Cues { track, cues }) if *track == id && cues.memories == after
    ));

    // Hot cue saves leave memory cues alone, labels and colours included.
    let mut hot = [None; earshot_engine::HOTCUES];
    hot[0] = Some(1.0);
    w.save_hotcues(id, &hot).unwrap();
    assert_eq!(read::memories(w.conn(), id).unwrap(), after);

    // Clearing goes back to the defaults; deleting takes the row, name and colour.
    w.set_memory_label(id, m[0].id, None).unwrap();
    w.set_memory_color(id, m[1].id, None).unwrap();
    assert_eq!(read::memories(w.conn(), id).unwrap(), m);
    w.set_memory_label(id, m[1].id, Some("Loop")).unwrap();
    w.delete_memory(id, m[1].id).unwrap();
    assert_eq!(read::memories(w.conn(), id).unwrap(), [m[0].clone()]);
    // A memory cue's id can't be used to name a hot cue's row.
    let hot_row: i64 = w
        .conn()
        .query_row("SELECT id FROM cues WHERE kind = 0", [], |r| r.get(0))
        .unwrap();
    w.set_memory_label(id, hot_row, Some("x")).unwrap();
    let labels = read::hot_labels(w.conn(), id).unwrap();
    assert!(labels.iter().all(Option::is_none));
}

#[test]
fn flows_save_load_move_and_delete() {
    let (_dir, mut w, t) = one_track("flows");
    w.create_collection(1, None, Folder, "Sets", None).unwrap();
    w.create_collection(2, Some(1), Folder, "Summer", None)
        .unwrap();
    assert!(
        matches!(w.move_to(1, Some(2)), Err(StoreError::Refused(_))),
        "no loops"
    );

    w.create_collection(3, Some(2), Flow, "Warm-up", None)
        .unwrap();
    // The same track twice is fine in a Flow.
    let g = graph(&[(1, t), (2, t)], &[(1, 2)]);
    w.save_flow(3, &g).unwrap();
    assert_eq!(read::flow(w.conn(), 3).unwrap().unwrap().2, g);
    // Saving again with one card moved writes only that (and still loads back).
    let mut moved = g.clone();
    moved.nodes[1].x = 50.0;
    w.save_flow(3, &moved).unwrap();
    assert_eq!(read::flow(w.conn(), 3).unwrap().unwrap().2, moved);

    w.duplicate(3, 4).unwrap();
    w.rename(4, "Peak").unwrap();
    w.move_to(4, None).unwrap();
    let all = read::collections(w.conn()).unwrap();
    assert_eq!(all.iter().filter(|c| c.kind == Folder).count(), 2);
    let peak = all.iter().find(|e| e.id == 4).unwrap();
    assert_eq!(
        (peak.name.as_str(), peak.parent, peak.tracks.len()),
        ("Peak", None, 2)
    );
    assert!(peak.linear);
    assert_eq!(read::flow(w.conn(), 4).unwrap().unwrap().2, moved, "copied");

    // Deleting a folder takes its subfolders and Flows with it; others stay.
    w.take_events();
    w.delete(1).unwrap();
    let mut gone = w
        .take_events()
        .into_iter()
        .find_map(|e| match e {
            StoreEvent::Deleted(ids) => Some(ids),
            _ => None,
        })
        .unwrap();
    gone.sort();
    assert_eq!(gone, vec![1, 2, 3]);
    let all = read::collections(w.conn()).unwrap();
    assert_eq!(all.iter().map(|e| e.id).collect::<Vec<_>>(), vec![4]);
    assert!(read::flow(w.conn(), 3).unwrap().is_none());
}

#[test]
fn a_flow_card_can_follow_several_but_not_loop() {
    let (_dir, mut w, t) = one_track("dag");
    w.create_collection(1, None, Flow, "F", None).unwrap();
    // 1 → 3 and 2 → 3: a join, which the old canvas couldn't keep.
    let join = graph(&[(1, t), (2, t), (3, t)], &[(1, 3), (2, 3)]);
    w.save_flow(1, &join).unwrap();
    assert_eq!(
        read::flow(w.conn(), 1).unwrap().unwrap().2.edges,
        join.edges
    );
    assert!(!read::collections(w.conn()).unwrap()[0].linear);
    let looped = graph(&[(1, t), (2, t)], &[(1, 2), (2, 1)]);
    assert!(matches!(
        w.save_flow(1, &looped),
        Err(StoreError::Refused(_))
    ));
    assert_eq!(
        read::flow(w.conn(), 1).unwrap().unwrap().2,
        join,
        "unchanged"
    );
}

#[test]
fn a_path_through_a_flow_exports_as_a_playlist() {
    let (dir, mut w, a) = one_track("export");
    scan(&mut w, &dir, &[file(&dir.join("music/b.mp3"), "B", None)]);
    let b = library(&mut w).iter().find(|t| t.id != a).unwrap().id;
    w.create_collection(1, None, Folder, "Sets", None).unwrap();
    w.create_collection(2, Some(1), Flow, "F", None).unwrap();
    // a → b → a, and a branch a → a.
    let g = graph(&[(1, a), (2, b), (3, a), (4, a)], &[(1, 2), (2, 3), (1, 4)]);
    w.save_flow(2, &g).unwrap();
    assert!(matches!(
        w.export_path(2, &[1, 3], 3, "Not a path"),
        Err(StoreError::Refused(_))
    ));
    w.export_path(2, &[1, 2, 3], 3, "Set A").unwrap();
    assert_eq!(read::collection_tracks(w.conn(), 3).unwrap(), vec![a, b, a]);
    let (parent, source): (Option<i64>, Option<i64>) = w
        .conn()
        .query_row(
            "SELECT parent_id, source_flow_id FROM collections WHERE id = 3",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((parent, source), (Some(1), Some(2)));
    // A snapshot: deleting the Flow keeps the playlist, without its source.
    w.delete(2).unwrap();
    assert_eq!(read::collection_tracks(w.conn(), 3).unwrap(), vec![a, b, a]);
}

#[test]
fn a_crate_holds_each_track_once() {
    let (_dir, mut w, t) = one_track("crate");
    w.create_collection(1, None, Crate, "Disco", None).unwrap();
    w.add_tracks(1, &[t, t]).unwrap();
    w.add_tracks(1, &[t]).unwrap();
    assert_eq!(read::collection_tracks(w.conn(), 1).unwrap(), vec![t]);
    w.remove_tracks(1, &[t]).unwrap();
    assert!(read::collection_tracks(w.conn(), 1).unwrap().is_empty());
    assert!(matches!(
        w.add_tracks(99, &[t]),
        Err(StoreError::NoCollection(99))
    ));
}

#[test]
fn playlists_keep_order_and_repeats() {
    let (dir, mut w, a) = one_track("playlist");
    scan(&mut w, &dir, &[file(&dir.join("music/b.mp3"), "B", None)]);
    let b = library(&mut w).iter().find(|t| t.id != a).unwrap().id;
    w.create_collection(1, None, Playlist, "P", None).unwrap();
    w.add_tracks(1, &[a, b]).unwrap();
    w.add_tracks(1, &[a]).unwrap();
    assert_eq!(read::collection_tracks(w.conn(), 1).unwrap(), vec![a, b, a]);
    w.remove_tracks(1, &[a]).unwrap();
    assert_eq!(read::collection_tracks(w.conn(), 1).unwrap(), vec![b]);
    w.add_tracks(1, &[a]).unwrap();
    assert_eq!(read::collection_tracks(w.conn(), 1).unwrap(), vec![b, a]);
    let listed = read::collections(w.conn()).unwrap();
    assert_eq!(listed[0].tracks, vec![b, a]);
}

#[test]
fn adding_to_a_flow_makes_starting_cards_and_removing_rejoins() {
    let (dir, mut w, a) = one_track("flow-add");
    scan(&mut w, &dir, &[file(&dir.join("music/b.mp3"), "B", None)]);
    let b = library(&mut w).iter().find(|t| t.id != a).unwrap().id;
    w.create_collection(1, None, Flow, "F", None).unwrap();
    // a → b → a
    w.save_flow(1, &graph(&[(1, a), (2, b), (3, a)], &[(1, 2), (2, 3)]))
        .unwrap();
    w.add_tracks(1, &[b]).unwrap();
    let g = read::flow_graph(w.conn(), 1).unwrap();
    assert_eq!(g.nodes.len(), 4);
    assert_eq!(g.parents(4).count(), 0, "a new starting card");
    // Taking b out removes both its cards; a → a is joined up.
    w.remove_tracks(1, &[b]).unwrap();
    let g = read::flow_graph(w.conn(), 1).unwrap();
    assert_eq!(g.nodes.iter().map(|n| n.id).collect::<Vec<_>>(), vec![1, 3]);
    assert_eq!(g.edges, vec![(1, 3)]);
}

#[test]
fn smart_playlists_run_their_query_in_sql_like_in_memory() {
    use crate::core::index::LibraryIndex;
    use crate::core::query;

    let (dir, mut w) = setup("smart");
    w.add_roots(&[dir.join("music")]).unwrap();
    let mut files = vec![];
    for (name, artist, bpm, key) in [
        ("Deep Space", "Daft", 124.0, "Am"),
        ("Shallow", "Daft", 128.0, "8A"),
        ("Other", "Someone", 140.0, "C"),
        ("Warm Up", "Someone", 122.0, "9A"),
    ] {
        let mut f = file(&dir.join(format!("music/{name}.mp3")), name, None);
        f.tags.artist = Some(artist.into());
        f.tags.bpm = Some(bpm);
        f.tags.key = Some(key.into());
        files.push(f);
    }
    scan(&mut w, &dir, &files);
    let lib = library(&mut w);
    let id = |title: &str| lib.iter().find(|t| t.title == title).unwrap().id;
    // A grid for one of them (so is:analysed and the grid's BPM count).
    w.save_user_grid(id("Other"), &grid(141.0)).unwrap();
    w.create_collection(9, None, Crate, "C", None).unwrap();
    w.add_tracks(9, &[id("Shallow"), id("Warm Up")]).unwrap();
    // Energy: Deep Space lowest, Warm Up highest.
    for (t, e) in [
        ("Deep Space", 1.0),
        ("Shallow", 2.0),
        ("Other", 3.0),
        ("Warm Up", 4.0),
    ] {
        save_features(w.conn(), id(t), &[("energy", e)], None).unwrap();
    }
    let mut ix = LibraryIndex::default();
    ix.replace(library(&mut w));
    let mut colls = crate::core::collections::Collections::default();
    colls.set(read::collections(w.conn()).unwrap());

    w.create_collection(1, None, Smart, "S", Some("")).unwrap();
    for q in [
        "",
        "daft",
        "\"deep space\"",
        "artist:someone",
        "title=shallow",
        "bpm:120-126",
        "bpm>=128",
        "bpm:141",
        "key:8A",
        "key:C",
        "-key:Am",
        "is:analysed",
        "-is:analysed",
        "in:9",
        "-in:9 daft",
        "energy>=7",
        "rating>=1",
        "-rating>=1",
        "daft OR warm",
        "(daft | someone) bpm<130",
        "time:300",
        "path:shallow",
        "sort:-bpm",
        "sort:title",
        "daft sort:-energy",
    ] {
        w.set_query(1, q).unwrap();
        let parsed = query::parse(q).unwrap();
        let sql = read::collection_tracks(w.conn(), 1).unwrap();
        let mem = ix.query(&parsed, &colls, None);
        if parsed.sort.0.is_empty() {
            let (mut a, mut b) = (sql.clone(), mem.clone());
            a.sort();
            b.sort();
            assert_eq!(a, b, "{q}");
        } else {
            assert_eq!(sql, mem, "{q}");
        }
    }
    assert!(matches!(
        w.set_query(1, "bpm:"),
        Err(StoreError::BadQuery(_))
    ));
}

/// The actor end to end: commands from "the UI", answers on the read pool and
/// events through the sink.
#[test]
fn the_store_thread_writes_and_reports() {
    let dir = std::env::temp_dir().join(format!("earshot-store-actor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    let events = Arc::new(Mutex::new(vec![]));
    let sink_events = events.clone();
    let store = Store::open(
        &dir.join(FILE_NAME),
        Arc::new(move |e| sink_events.lock().unwrap().push(e)),
    )
    .unwrap();
    store.send(StoreCmd::AddRoots(vec![dir.join("music")]));
    let roots = store.call(|w| w.roots()).unwrap();
    assert_eq!(roots, vec![dir.join("music")]);
    let f = file(&dir.join("music/a.mp3"), "A", None);
    let root = dir.join("music");
    store
        .call(move |w| w.apply_scan(&[root], &[f.path.clone()], &[f]))
        .unwrap();
    let id = store.reader.with(|c| read::tracks(c, None)).unwrap()[0].id;
    assert_eq!(store.reader.track(id).unwrap().title, "A");

    let flow = store.new_collection_id();
    store.send(StoreCmd::Create {
        id: flow,
        parent: None,
        kind: Flow,
        name: "F".into(),
        query: None,
    });
    store.send(StoreCmd::SaveFlow {
        id: flow,
        graph: graph(&[(1, id)], &[]),
    });
    store.send(StoreCmd::LoadFlow(flow));
    store.send(StoreCmd::Rename {
        id: 999,
        name: "nothing".into(),
    });
    store.call(|_| Ok(())).unwrap(); // wait for the queue
    let events = events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StoreEvent::Roots(r) if r.len() == 1))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StoreEvent::Library(l) if l.len() == 1))
    );
    assert!(events.iter().any(
        |e| matches!(e, StoreEvent::FlowLoaded(Some((f, _, g))) if *f == flow && g.nodes.len() == 1)
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StoreEvent::Error(StoreError::NoCollection(999))))
    );
    assert_ne!(store.new_collection_id(), flow);
}

/// Bug 6: editing a grid doesn't wait for the database (it used to write while
/// holding the player lock, on the UI thread).
#[test]
fn a_grid_edit_doesnt_wait_for_the_database() {
    use crate::core::player::{self, GridOp};
    use earshot_engine::OutputOptions;
    use earshot_engine::decode::TrackAudio;

    let dir = std::env::temp_dir().join(format!("earshot-store-gridlock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("music")).unwrap();
    let store = Store::open(&dir.join(FILE_NAME), Arc::new(|_| {})).unwrap();
    let root = dir.join("music");
    let f = file(&root.join("a.mp3"), "A", None);
    store
        .call(move |w| {
            w.add_roots(std::slice::from_ref(&root))?;
            w.apply_scan(&[root], &[f.path.clone()], &[f])
        })
        .unwrap();
    let id = store.reader.with(|c| read::tracks(c, None)).unwrap()[0].id;

    let p = player::new_player();
    player::start_engine(
        &p,
        &OutputOptions {
            device: None,
            buffer_frames: Some(256),
            null: true,
        },
    )
    .unwrap();
    {
        let mut inner = p.lock().unwrap();
        inner.decks[0].track_id = Some(id);
        inner.decks[0].audio = Some(Arc::new(TrackAudio {
            samples: vec![0.0; 2 * 44100 * 20],
            sample_rate: 44100,
        }));
    }
    // Keep the writer busy, as if a big scan were being written.
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let busy = store.clone();
    let blocker = std::thread::spawn(move || {
        busy.call(move |_| {
            let _ = wait.recv();
            Ok(())
        })
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    let t = std::time::Instant::now();
    let deck = earshot_engine::DeckId::new(0).unwrap();
    player::edit_grid(&p, &store, deck, GridOp::SetBpm(127.0)).unwrap();
    assert!(t.elapsed() < std::time::Duration::from_millis(500));
    assert!(p.try_lock().is_ok(), "the player lock isn't held");
    release.send(()).unwrap();
    blocker.join().unwrap().unwrap();
    store.call(|_| Ok(())).unwrap();
    let t = store.reader.track(id).unwrap();
    assert_eq!(
        (t.bpm, t.grid_source),
        (Some(127.0), Some(GridSource::User))
    );
    // Saved as a steady grid's numbers: the new tempo, the 1 where it was.
    let (model, _) = store.reader.grid(id).unwrap().unwrap();
    assert!(matches!(model, GridModel::Constant(c) if c.bpm == 127.0));
    // Nudged and anchored: the anchor moves, the tempo stays exactly as typed.
    player::edit_grid(&p, &store, deck, GridOp::Nudge(-10.0)).unwrap();
    player::edit_grid(&p, &store, deck, GridOp::AnchorHere).unwrap();
    store.call(|_| Ok(())).unwrap();
    let (model, _) = store.reader.grid(id).unwrap().unwrap();
    let GridModel::Constant(c) = model else {
        panic!("{model:?}")
    };
    assert_eq!((c.bpm, c.anchor_pos), (127.0, 1));
}
