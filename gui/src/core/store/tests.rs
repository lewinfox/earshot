use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use earshot_engine::beatgrid::Beatgrid;
use earshot_engine::decode::TrackTags;

use super::*;
use crate::core::flows::Card;

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
    w.create_collection(500, None, "flow", "Set", None).unwrap();
    w.save_flow(500, &[card(1, id, None)]).unwrap();
    id
}

fn card(id: u64, track_id: i64, parent: Option<u64>) -> Card {
    Card {
        id,
        track_id,
        parent,
        x: id as f32,
        y: 0.0,
    }
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
        read::flow(w.conn(), 500).unwrap().unwrap().2[0].track_id,
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
        read::flow(w.conn(), 500).unwrap().unwrap().2[0].track_id,
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
            bpm,
            expected_active: expected,
            force,
        }),
    }
}

fn one_track(name: &str) -> (PathBuf, Writer, i64) {
    let (dir, mut w) = setup(name);
    w.add_roots(&[dir.join("music")]).unwrap();
    scan(&mut w, &dir, &[file(&dir.join("music/a.mp3"), "A", None)]);
    let id = library(&mut w)[0].id;
    (dir, w, id)
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
    assert_eq!((g.len(), src), (grid(125.0).len(), GridSource::Analysis));
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
        |e| matches!(e, StoreEvent::Memories { track, memories } if *track == id && !memories.is_empty())
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
    assert_eq!(read::memories(w.conn(), id).unwrap(), [m[1]]);
    assert_eq!(read::hotcues(w.conn(), id).unwrap(), hot);
}

#[test]
fn flows_save_load_move_and_delete() {
    let (_dir, mut w, t) = one_track("flows");
    w.create_collection(1, None, "folder", "Sets", None)
        .unwrap();
    w.create_collection(2, Some(1), "folder", "Summer", None)
        .unwrap();
    assert!(
        matches!(w.move_to(1, Some(2)), Err(StoreError::Refused(_))),
        "no loops"
    );

    w.create_collection(3, Some(2), "flow", "Warm-up", None)
        .unwrap();
    // The same track twice is fine in a Flow.
    let cards = vec![card(1, t, None), card(2, t, Some(1))];
    w.save_flow(3, &cards).unwrap();
    assert_eq!(read::flow(w.conn(), 3).unwrap().unwrap().2, cards);
    // Saving again with one card moved writes only that (and still loads back).
    let mut moved = cards.clone();
    moved[1].x = 50.0;
    w.save_flow(3, &moved).unwrap();
    assert_eq!(read::flow(w.conn(), 3).unwrap().unwrap().2, moved);

    w.duplicate_flow(3, 4).unwrap();
    w.rename(4, "Peak").unwrap();
    w.move_to(4, None).unwrap();
    let (folders, flows) = read::flows(w.conn()).unwrap();
    assert_eq!(folders.len(), 2);
    let peak = flows.iter().find(|e| e.id == 4).unwrap();
    assert_eq!(
        (peak.name.as_str(), peak.folder, peak.cards),
        ("Peak", None, 2)
    );
    assert!(peak.linear);

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
    let (folders, flows) = read::flows(w.conn()).unwrap();
    assert!(folders.is_empty());
    assert_eq!(flows.iter().map(|e| e.id).collect::<Vec<_>>(), vec![4]);
    assert!(read::flow(w.conn(), 3).unwrap().is_none());
}

#[test]
fn a_flow_can_branch_but_not_loop() {
    let (_dir, mut w, t) = one_track("dag");
    w.create_collection(1, None, "flow", "F", None).unwrap();
    let branch = vec![card(1, t, None), card(2, t, Some(1)), card(3, t, Some(1))];
    w.save_flow(1, &branch).unwrap();
    assert!(!read::flows(w.conn()).unwrap().1[0].linear);
    let looped = vec![card(1, t, Some(2)), card(2, t, Some(1))];
    assert!(matches!(
        w.save_flow(1, &looped),
        Err(StoreError::Refused(_))
    ));
    assert_eq!(
        read::flow(w.conn(), 1).unwrap().unwrap().2,
        branch,
        "unchanged"
    );
}

#[test]
fn a_path_through_a_flow_exports_as_a_playlist() {
    let (dir, mut w, a) = one_track("export");
    scan(&mut w, &dir, &[file(&dir.join("music/b.mp3"), "B", None)]);
    let b = library(&mut w).iter().find(|t| t.id != a).unwrap().id;
    w.create_collection(1, None, "folder", "Sets", None)
        .unwrap();
    w.create_collection(2, Some(1), "flow", "F", None).unwrap();
    // a → b → a, and a branch a → a.
    let cards = vec![
        card(1, a, None),
        card(2, b, Some(1)),
        card(3, a, Some(2)),
        card(4, a, Some(1)),
    ];
    w.save_flow(2, &cards).unwrap();
    assert!(matches!(
        w.export_path(2, &[1, 3], 3, "Not a path"),
        Err(StoreError::Refused(_))
    ));
    w.export_path(2, &[1, 2, 3], 3, "Set A").unwrap();
    assert_eq!(read::playlist(w.conn(), 3).unwrap(), vec![a, b, a]);
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
    assert_eq!(read::playlist(w.conn(), 3).unwrap(), vec![a, b, a]);
}

#[test]
fn a_crate_holds_each_track_once() {
    let (_dir, mut w, t) = one_track("crate");
    w.create_collection(1, None, "crate", "Disco", None)
        .unwrap();
    w.add_to_crate(1, &[t, t]).unwrap();
    w.add_to_crate(1, &[t]).unwrap();
    let n: i64 = w
        .conn()
        .query_row("SELECT count(*) FROM crate_tracks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
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
    store.send(StoreCmd::CreateFlow {
        id: flow,
        folder: None,
        name: "F".into(),
    });
    store.send(StoreCmd::SaveFlow {
        id: flow,
        cards: vec![card(1, id, None)],
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
        |e| matches!(e, StoreEvent::FlowLoaded(Some((f, _, cards))) if *f == flow && cards.len() == 1)
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
}
