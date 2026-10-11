# Data model and persistence

Part of the [October 2026 architecture review](README.md). Line references are to `8bb7c31`.

## 1. Current model

**Schema.** Six migrations (`gui/migrations/001–006`) create these tables:

| Table | Contents |
|---|---|
| `library_folders` | Folders added to the library |
| `tracks` | `path` (UNIQUE), `size`, `mtime`, `duration`, `title`, `artist`, `tag_bpm`, `tag_key`. Later migrations added `energy`, `loudness` and `peak`. |
| `analysis` | One row per track: `bpm`, `min`, `max`, `detector`, `mode`, `drifts` |
| `beatgrids` | One row per track: a blob and a free-text `source` |
| `waveforms` | One row per track: `data`, `bins_per_s`, `version`, `preview` |
| `flow_folders`, `flows`, `flow_cards` | Saved Flows |

There are no tables for cues, tags, playlists or crates, play history, embeddings, external IDs, or jobs. Only about half of the sketch in `docs/design.md:204-219` exists.

**Database access.**
- There is one connection: `Db = Arc<Mutex<Connection>>` (`db.rs:13`).
- Many threads share it: the UI thread, the scan thread, the art thread, up to 5 analysis lanes, and one thread per deck load or preview.
- Every function returns `Result<_, String>`.

**In-memory model.**
- `db::LibTrack` (`db.rs:146`) is wrapped by `ui::library_ui::Track` (`library_ui.rs:20`). The wrapper adds the parsed key, the energy rank and the analysis status.
- Several other types hold partial copies of track data: `PendingLoad` and `DeckInfo` (`deck.rs:48-86`), `Candidate` (`suggest.rs:40`) and `RefInfo` (`suggest_ui.rs:32`).
- Filtering runs in memory over a `Vec<Track>` (`library_ui.rs:191-207`).
- The track table cannot be sorted by column.

**eframe storage** holds:
- audio settings,
- the waveform zoom,
- tempo ranges,
- the open Flow's id and its camera position,
- an old list of cards kept only for migration (`app.rs:73-79, 1073-1077`, `flow.rs:53-63`).

## 2. Findings, most important first

### F1. There is no single writer, and the UI thread does blocking database work

- `design.md:197-199` promises "a single DB thread". In practice, one mutex-guarded connection is locked from at least 8 kinds of thread:
  - `library.rs:102,167,189`
  - `analysis.rs:265,280,287,292,296,354`
  - `player.rs:135,140,200,205,218,245,341`
  - `app.rs:209,255,279,291,312,672`
- `scan_library` holds the lock for the whole `upsert_tracks` transaction and then `all_tracks` (`library.rs:167-174`). On a first import of 20k files, the UI thread stalls at its next `save_flow` or `refresh_previews`.
- Every `.lock().unwrap()` turns one panic into a poisoned mutex. After that, every thread that touches the DB crashes.

**Fix:**
- Add a `Store` actor that owns the only writing connection and takes commands over a channel.
- Give it a small pool of read-only connections for loaders. SQLite's WAL mode allows this.
- The UI thread should never call SQLite.

### F2. `previews()` scans the whole table and writes during a read, on the UI thread

- `db.rs:341-377` loads every preview blob, then filters by `ids` in Rust.
- It is called each time an analysis finishes (`app.rs:762`) and after each load (`app.rs:713`).
- With 50k tracks, that is about 25 MB read on the UI thread for every analysed track.
- It also writes missing previews back to the DB (`db.rs:367`).
- It ignores `version`, so old thumbnails survive a `WAVEFORM_VERSION` bump.

**Fix:** query with `WHERE track_id IN (…)`, and create previews only on the writer.

### F3. A track's BPM comes from three places that drift apart

- The library shows `analysis.bpm` if there is one, otherwise `tag_bpm` (`library_ui.rs:50-52`, `deck.rs:82`).
- `edit_grid` saves a `"user"` grid but never updates `analysis.bpm` (`player.rs:341`).
- `app.rs:1223` doesn't update the library row either.

So after you fix a BPM by hand, the library, suggestions and search keep showing the old tempo indefinitely.

There is a second problem in the other direction. When a user grid is kept, analysis still overwrites `analysis.bpm` with the auto-detected value (`analysis.rs:355` vs `357-358`).

**Fix:** take BPM from the *active* grid. Store it next to the grid as a copied column, written in the same transaction.

### F4. A hand-fixed grid can be lost to a race

- Whether to keep the user's grid (`keep_user`) is decided from `existing`, which is read *before* the job runs (`analysis.rs:265`, `353`). The worker then runs for several seconds. If you edit the grid during that time, `save_grid` overwrites your edit and `update_decks` pushes the overwritten grid to the deck.
- A forced re-analysis (`app.rs:663`, `force=true`) always overwrites user grids. That contradicts `design.md:158-159`: "re-analysis never overwrites a hand-fixed grid".

**Fix:** either of these:
- Keep grids as versioned rows, with an `active_grid_id` that only the user, or an explicit "accept", can change.
- Or do a compare-and-set on `updated_at` inside the writer.

### F5. Grid source and analysis mode are free-text strings, recorded twice

- Grid source is free text: `"analysis"`, `"analysis (dynamic)"`, `"user"`, `"tempo-lock sidecar"` (`analysis.rs:46`, `player.rs:379,386`).
- The mode is recorded both in that string and in `analysis.mode` (`004_analysis_mode.sql:3`, `db.rs:257-264`).
- The list of sources that count as "analysed" is copied in `library_ui.rs:18` and `player.rs:256-259`.
- `analysis.detector` stands in for an analyser version.

**Fix:** use `enum GridSource { Analysis{mode}, User, Import(ExternalLib), Sidecar }`. Store it as an integer or as TEXT with a CHECK constraint, and define it in one place.

### F6. Analysis results are spread across tables and rewritten on every load

- `energy`, `loudness` and `peak` are on `tracks` (migrations `002` and `006`), not on `analysis` as `design.md:208-209` planned.
- `load_track` measures them again and writes them on *every* deck load (`player.rs:203-207`). The analysis job also writes them (`analysis.rs:286-290`).
- Energy is converted to a rank in UI code (`library_ui.rs:146-161`), so the suggestion scorer's input depends on the UI.

**Fix:** put all derived features in one `track_features` (or `analysis_runs`) table, with the analyser version. Compute the rank or percentile in core.

### F7. A track is identified by its path string

- `tracks.path` is `UNIQUE` (`001:12`) and is built with `to_string_lossy` (`library.rs:125,147`), so file names that aren't valid UTF-8 are corrupted.
- A moved or renamed file becomes a new track. It loses its grid, analysis and Flow membership. The old row stays forever, hidden (`library.rs:169-174`).
- There is no `missing` flag, no content hash, and no way to relocate a file.
- Deleting a track row silently deletes its Flow cards through `ON DELETE CASCADE` (`005:20`). That breaks the chain, because `parent_card` has no foreign key.

**Fix:**
- The track's identity is its `id`.
- Store a `file_id` with an audio-content hash, for example a hash of the first N decoded frames, or the size plus a hash of the first and last 64 KB.
- Treat the path as a location that can change, with a `missing_since` column.
- Relocate files by matching on the hash.

### F8. Tag metadata is thin, has no record of where it came from, and can't be edited safely

- Only title, artist, BPM, key and duration are read from tags (`engine/src/decode.rs:33-39`).
- A rescan overwrites title and artist whenever mtime or size change (`db.rs:129-131`). Any user edit stored in those columns would be lost.
- The title falls back to the file name in the *DB layer* (`db.rs:176-185`), so a track with no title can't be told apart from one whose title is the file name.
- `load_track` reads the title again from the file tags (`player.rs:229-237`). Once editing exists, the deck could show a different title from the library.

**Fix:**
- Keep values read from the file separate from values the user set. Either use `tag_*` and `user_*` column pairs, or a key/value table.
- Resolve them in one place (`COALESCE(user, tag)`), through a view or the repository layer.
- Apply display fallbacks such as the file name in the UI.

### F9. Flows are doing the job of playlists, and saving them is expensive

- A "playlist" is any Flow whose cards happen to form a single chain (`flows.rs:47-73`).
- cratelib sync needs explicitly ordered playlists, unordered crates and smart queries. A graph with x/y positions is the wrong form to store them in. One stray link turns a playlist into a non-playlist.
- `flows::list` loads every card of every Flow, one query per Flow, just to work out whether each is linear (`flows.rs:101-111`). It runs on the UI thread after every save (`app.rs:304`).
- `save` deletes and re-inserts every card (`flows.rs:235-264`).

**Fix:**
- Make `playlists` and `playlist_entries` the stored form, with a kind of playlist, crate, smart or flow.
- Store the Flow layout (x, y and edges) as a separate layer, keyed by entry.
- Store `is_linear`, or compute it in SQL.

### F10. Saved state is split across two stores that refer to each other

- The open Flow's id is a DB primary key, but it is kept in eframe storage (`flow.rs:59-60`).
- `EARSHOT_DATA_DIR` switches the DB but not eframe storage, so that id can point at an unrelated Flow in another DB.
- The camera position is stored once for the whole app, not per Flow.
- The code that migrates old cards (`app.rs:250-276`, `flow.rs:55-57`) can go, since nothing has been deployed.

**Fix:**
- Store document state, such as each Flow's camera and the last-open Flow, in a `settings` table in the DB or on the `flows` row.
- Keep only device and window preferences in eframe storage.

### F11. Play history isn't saved

- "Played" is a `HashSet` that lasts for one session only (`library_ui.rs:121`, `app.rs:436-455`).
- `design.md:216` plans `sets` and `set_plays` tables. The scoring at `:235` and Phase 3 depend on them.

### F12. Hot cues and keys have no stored form

- Hot cues exist only in engine state (`wave.rs:118`), and are lost when the deck reloads.
- The key is the raw tag text, parsed in UI code (`library_ui.rs:38`). There is no analysed key, no confidence and no record of its source.

**Fix:** add a `cues` table now. Store the key as an integer pitch class plus a mode, with its source.

### F13. Filtering is hard-coded and lives in the UI

- `Query::matches(title, artist, bpm, key)` (`util.rs:148-168`) is in `ui`. Every new searchable field means changing its signature.
- It allocates a lowercase string for every row each time the filter runs.

**Fix:**
- Filtering 50k rows in memory is fine. Move the predicate into a core `TrackFilter` that works on a `TrackView` with a precomputed lowercase `search_text`.
- Smart playlists should reuse the same AST, saved as JSON.

### F14. Migrations and error handling work, but are weak

- The `user_version` migration runner (`db.rs:41-54`) is fine. Squash 001–006 into one baseline.
- Add:
  - `PRAGMA busy_timeout`,
  - `synchronous=NORMAL`,
  - an index on `flow_cards(track_id)`, which the cascade from `tracks` needs,
  - `STRICT` tables.
- Replace `String` errors with a `thiserror` `DbError`.
- Blob versioning is inconsistent:
  - the grid blob has a magic number and a version,
  - the waveform has only a `version` column,
  - the preview has none.

  Give every kind of blob a format-version column.

## 3. Proposed schema (one squashed migration, `001`)

```sql
CREATE TABLE files(            -- physical location; many per track over time
  id INTEGER PRIMARY KEY, track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  path BLOB NOT NULL UNIQUE,   -- OsStr bytes
  size INTEGER, mtime_ns INTEGER, content_hash BLOB, missing_since INTEGER);
CREATE INDEX files_hash ON files(content_hash);

CREATE TABLE tracks(
  id INTEGER PRIMARY KEY, added_at INTEGER NOT NULL,
  duration_s REAL, sample_rate INTEGER,
  derived_from INTEGER REFERENCES tracks(id), derivation TEXT,   -- tempo-lock renders
  active_grid_id INTEGER REFERENCES beatgrids(id),
  bpm REAL, key_pc INTEGER, key_minor INTEGER,                   -- denormalised "effective" values
  rating INTEGER, color INTEGER, deleted_at INTEGER) STRICT;

CREATE TABLE track_meta(       -- tag vs user provenance, any field
  track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
  field TEXT NOT NULL,          -- title, artist, album, genre, comment, label, year, bpm, key …
  source TEXT NOT NULL,         -- 'tag' | 'user' | 'import:rekordbox' …
  value TEXT, updated_at INTEGER,
  PRIMARY KEY(track_id, field, source)) STRICT;
-- plus a VIEW track_resolved picking user > import > tag, and an FTS5 table over it

CREATE TABLE analysis_runs(id INTEGER PRIMARY KEY, track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
  analyser TEXT, version TEXT, mode TEXT, started_at INTEGER, finished_at INTEGER, error TEXT);
CREATE TABLE track_features(track_id INTEGER, run_id INTEGER REFERENCES analysis_runs(id),
  name TEXT, value REAL, PRIMARY KEY(track_id, name));   -- energy, lufs, peak, bpm_min/max, drifts, key_conf
CREATE TABLE beatgrids(id INTEGER PRIMARY KEY, track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
  source INTEGER NOT NULL, run_id INTEGER, beats BLOB NOT NULL, created_at INTEGER);
CREATE TABLE blobs(track_id INTEGER, kind TEXT, format_version INTEGER, params TEXT, data BLOB,
  PRIMARY KEY(track_id, kind));     -- waveform, waveform_preview, art_thumb
CREATE TABLE embeddings(track_id INTEGER, model TEXT, dim INTEGER, vector BLOB, PRIMARY KEY(track_id, model));

CREATE TABLE cues(id INTEGER PRIMARY KEY, track_id INTEGER REFERENCES tracks(id) ON DELETE CASCADE,
  kind INTEGER, slot INTEGER, start_s REAL, end_s REAL, label TEXT, color INTEGER, source TEXT,
  UNIQUE(track_id, kind, slot));
CREATE TABLE tags(id INTEGER PRIMARY KEY, name TEXT, category TEXT, UNIQUE(category, name));
CREATE TABLE track_tags(track_id INTEGER, tag_id INTEGER, source TEXT, confidence REAL,
  PRIMARY KEY(track_id, tag_id));

CREATE TABLE collections(id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
  kind TEXT CHECK(kind IN ('folder','playlist','crate','smart','flow')), name TEXT,
  query TEXT, sort_order INTEGER, view_state TEXT, created_at INTEGER, updated_at INTEGER);
CREATE TABLE collection_entries(id INTEGER PRIMARY KEY, collection_id INTEGER REFERENCES collections(id) ON DELETE CASCADE,
  track_id INTEGER REFERENCES tracks(id) ON DELETE RESTRICT, position REAL,
  x REAL, y REAL);                      -- x/y only for flows
CREATE TABLE flow_edges(collection_id INTEGER, from_entry INTEGER, to_entry INTEGER,
  PRIMARY KEY(collection_id, from_entry, to_entry));

CREATE TABLE sessions(id INTEGER PRIMARY KEY, started_at INTEGER, ended_at INTEGER);
CREATE TABLE plays(session_id INTEGER, track_id INTEGER, deck INTEGER, started_at INTEGER, ended_at INTEGER, counted INTEGER);
CREATE TABLE external_links(track_id INTEGER, library TEXT, external_id TEXT, synced_at INTEGER, etag TEXT,
  PRIMARY KEY(library, external_id));
CREATE TABLE jobs(id INTEGER PRIMARY KEY, kind TEXT, track_id INTEGER, status TEXT, error TEXT, created_at INTEGER, finished_at INTEGER);
CREATE TABLE library_roots(id INTEGER PRIMARY KEY, path BLOB UNIQUE);
CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT);
```

### Rust domain model (`core::model`, `core::store`)

```rust
pub struct TrackId(i64);
pub struct Track {                 // the one canonical in-memory row
    pub id: TrackId, pub location: Option<PathBuf>, pub missing: bool,
    pub meta: Meta,                // resolved title/artist/album/genre/comment/year (Option each)
    pub duration: Option<f64>, pub bpm: Option<f64>, pub key: Option<Key>,
    pub grid_source: Option<GridSource>, pub energy: Option<f32>, pub loudness: Option<Loudness>,
    pub rating: Option<u8>, pub color: Option<u32>, pub tags: SmallVec<[TagId; 4]>,
    pub search_text: Box<str>,     // lowercase, precomputed
}
pub enum GridSource { Analysis(Mode), User, Import(ExternalLib), Sidecar, TagPlaceholder }
pub enum Field { Title, Artist, Album, Genre, Comment, Year, Bpm, Key, ... }
pub enum Provenance { Tag, User, Import(ExternalLib) }

pub struct LibraryIndex { tracks: Vec<Track>, by_id: HashMap<TrackId, usize>, rev: u64 }
pub struct TrackFilter { /* AST: And/Or/Field op value; serde for smart playlists */ }
impl LibraryIndex { fn query(&self, f: &TrackFilter, sort: &[SortKey]) -> Vec<TrackId>; }

pub enum StoreCmd {            // single writer thread
    UpsertScanned(Vec<ScannedFile>), SetMeta(TrackId, Field, Option<String>),
    SaveGrid { id: TrackId, grid: Beatgrid, source: GridSource, if_unchanged_since: Option<i64> },
    SaveFeatures(TrackId, RunId, Features), SaveCues(TrackId, Vec<Cue>),
    Collection(CollectionCmd), RecordPlay(Play), ...
}
pub enum StoreEvent { TracksChanged(Vec<Track>), CollectionsChanged, ... }  // UI patches LibraryIndex
pub struct Reader { pool: r2d2-like Vec<Connection> }  // read-only, for loaders/previews
```

Two rules for using this model:
- The UI keeps short-lived state (queued, running, error) in a separate map keyed by `TrackId`, not inside `Track`.
- `PendingLoad`, `Candidate` and `RefInfo` borrow from `Track` or copy from it. They should not work out fields such as `bpm.or(tag_bpm)` themselves.

## 4. Issues for other areas

- **Engine and player.**
  - `load_track` writes to the DB and decides analysis policy (`player.rs:195-260`).
  - `edit_grid` writes to the DB while holding the player lock (`player.rs:303,341`).
  - Move both into the store and analysis services.
- **Analysis.**
  - Results arrive as UI events that update the library piece by piece (`app.rs:743-762`), and grid edits don't notify the library at all. Replace both with one `TracksChanged` event from the store.
  - Add a saved `jobs` table so the queue survives a restart (`design.md:218`).
- **UI.**
  - The track table can't be sorted by column.
  - Folder filtering uses a string-prefix match (`library_ui.rs:199`).
  - The search logic is in `ui/util.rs`.
- **Worker and cratelib.** There is no `external_links` table and no record of which library a value was imported from. Both are needed before any sync work.
- **Flow UI.** Saving when a gesture ends is fine. But each save rewrites every card and then runs a query per Flow, on the UI thread (`app.rs:286-307`, `flows.rs:101`).
