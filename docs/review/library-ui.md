# Library, curation and set-building UI

Part of the [October 2026 architecture review](README.md). Line references are to `8bb7c31`.

## 1. Current structure

### Data path
- `core::library::scan_library` walks the filesystem, upserts into `tracks`, and returns every `LibTrack` (`core/db.rs:146-199`).
- The UI keeps the whole library in memory as `LibraryUi.tracks: Vec<Track>` (`ui/library_ui.rs:20-32, 99-124`).
- `Track` wraps `LibTrack` and adds fields the UI works out itself: the parsed key, the energy rank, whether the track is analysed or dynamic, and its queue status.

### `LibraryUi` does too many jobs
It holds all of these:
- the track store and the folder-tree cache,
- the search box and the selection,
- art and thumbnail textures, and waveform previews,
- the session's "played" set.

`SuggestUi` (`suggest_ui.rs:84`) and `FlowUi` (`flow.rs:383`) take `&LibraryUi` as their only source of track data.

### Track lists
There are three hand-built track lists, plus a smaller fourth:
- the library table (`library_ui.rs:448-554`),
- the suggestions table (`suggest_ui.rs:164-259`),
- Flow cards and pick tiles (`flow.rs:938+`, `flow.rs:1320+`),
- the Flow search popup (`flow.rs:846-906`).

### Collections
- **Library folders** are filesystem paths (`library_folders`).
- **Saved Flows** are `flow_folders` / `flows` / `flow_cards`: a graph with x/y positions (`migrations/005_flows.sql`).
- **Playlists** are not stored anywhere. A playlist is a Flow that `flows::playlist_order` (`core/flows.rs:262`) can walk as one chain.
- `docs/design.md` sketched `crates(id,name,parent_id,kind,query JSON)` and `crate_tracks`, but the code went a different way.

### Search
An in-memory linear filter (`ui/util.rs:491-547`), re-run on every edit (`library_ui.rs:191-207`).

## 2. Findings (most important first)

### F1. A track is identified by its file path, and missing files disappear
**Problem.**
- `scan_library` returns only the files it found (`core/library.rs:171-174`). There is no `missing` flag, and the `file_hash` from the design was never built.
- A moved or renamed file becomes a new `tracks.id` with no analysis.
- Flow cards still point at the old id and are silently not drawn (`flow.rs:462`). Edges to those cards are still drawn (`flow.rs:744-751`).
- The card counts in the tree no longer match the canvas (`flows.rs:323` vs `flow.rs:680-684`).

**Impact.** Relocating missing files, finding duplicates and cratelib sync can't be built on this.

**Recommendation.**
- Keep tracks across scans and set `missing_since` instead of dropping them.
- Add a content fingerprint: size plus a hash of the first and last N KB, or an audio fingerprint.
- Add a relocate flow that changes `path` and keeps the id.

### F2. Rescans overwrite metadata, and the schema has no curation fields
**Problem.**
- `upsert_tracks` replaces title, artist, BPM and key from the file tags whenever size or mtime change (`db.rs:127-131`). Any edit made in the app would be lost.
- There is no album, genre, year, comment, rating, colour, tags or `date_added` in the UI. Tag reading covers only title, artist, BPM and key (`library.rs:146-155`).
- The display fallback (title = file stem) is applied in the DB layer (`db.rs:176-185`). An empty title then can't be told apart from a real one.

**Recommendation.**
- Split the data in two:
  - `file_tags`: what was last read from disk.
  - `track_meta`: the user's values. Nullable, and they win over `file_tags`.
- Add a `write_back` policy for each field.
- Apply display fallbacks in the view layer.

### F3. `LibraryUi` is the domain model, and business logic sits in UI files
**Problem.** None of the following can be tested without egui:
- Energy ranking: `library_ui.rs:146-161`.
- Whether a track counts as analysed, decided by a list of grid-source strings: `library_ui.rs:18, 40`.
- Analysis events changing `t.lib.bpm` / `drifts` / `dynamic` in place: `app.rs:735-744`. Energy is not re-ranked after analysis.
- The Flow graph model lives in `ui/flow.rs:231-371`: adding a child, removing a card and rejoining its neighbours, linking with a loop check, and reflow.
- `core/flows.rs` has its own `Card` type alongside the UI one (`flows.rs:31` vs `flow.rs:38`).
- The loop check is written twice: once for the graph (`flow.rs:295`) and once for folders (`flows.rs:148` and `flows_ui.rs:280`).
- The query parser is in `ui/util.rs`.
- The "played this session" set is on `LibraryUi` (`library_ui.rs:121`).

**Recommendation.** Move these into core:
- a `Library` store: track index, derived fields, version counter,
- a `FlowGraph`,
- a `query` module.

### F4. Every track list is hand-built, and the context menus disagree
**Problem.** The menus offer different actions in each list:

| List | Actions |
|---|---|
| Library row (`library_ui.rs:594-693`) | Double-click loads. Menu: Load A/B, Preview, Add to Flow, Analyse. |
| Library multi-selection (`:601-646`) | A completely different menu, with no Load or Preview. |
| Suggestion row (`suggest_ui.rs:244-258`) | Preview and Load only, in a different order. No Add to Flow, no Analyse. |
| Flow card (`flow.rs:971-990`) | Load, Unlink, Remove. No Preview, Analyse or "show in library". |

Shared pieces are also copied in each list instead of being reused:
- The title/artist cell, the "played = dimmed" rule and the key chip appear in `library_ui.rs:494-509`, `suggest_ui.rs:185-205` and `flow.rs:1028-1034`.
- Waveform thumbnail textures are cached twice (`library_ui.rs:117`, `flow.rs:101`).

**Impact.** Every new action (rate, tag, add to playlist, reveal in folder, edit) would have to be added in four places.

### F5. Selection works differently in each pane, and keyboard and suggestions can't see the Flow
**Problem.** Each pane has its own selection model:
- **Library:** `chosen: HashSet<i64>`, plus `focus` and `anchor` as indices into `shown` (`library_ui.rs:109-112`).
- **Suggestions:** a focus index only, with no multi-select. A row only looks selected while the pane has focus (`suggest_ui.rs:183`).
- **Flow:** `selected: Option<u64>`, holding a card id (`flow.rs:99`).
- **Flows tree:** no selection at all.

`Pane` can only be Library or Suggest (`app.rs:68-71`). This has two effects:
- With the Flow tab open, arrow keys, "load selected" and MIDI browse still drive the hidden library list (`app.rs:414-426`).
- Suggestions set to *Selected* follow `library.focused()` and ignore the selected card (`app.rs:482`).

### F6. Drag-and-drop payloads are untyped and carry one track
**Problem.**
- Every track source sends a bare `i64` (`library_ui.rs:552`, `suggest_ui.rs:243`, `flow.rs:964`). Dragging a multi-selection drags only one track.
- Flow cards use the same payload type. The canvas needs a `moving.is_none()` workaround to avoid adding a card twice (`flow.rs:511-513`).
- Tracks can't be dropped onto a Flow in the tree, because `drop_target` only accepts `Item` (`flows_ui.rs:308`).

**Recommendation.** Use one payload type: `DragPayload::Tracks { ids: Vec<TrackId>, origin: Origin }`.

### F7. Flows, playlists and folders are mixed up
**Problem.**
- A playlist is inferred from the shape of the graph. Order comes from the edges, so the same tracks can count as a playlist or not depending on how they are linked.
- "Add N to Flow" adds N unlinked root cards (`app.rs:1238-1243`). Adding a multi-selection to a "playlist" therefore turns it into a non-playlist.
- A track's menu has no way to add it to a named collection. `AddToFlow` always targets whichever Flow is open, or quietly creates one called "Untitled" (`app.rs:286-300`).
- The left tree uses "folder" for two unrelated things: filesystem folders (`library_ui.rs:246-266`) and virtual Flow folders (`flows_ui.rs`).
- A Flow can't be viewed as a track list. TODO §3 notes this list view as missing.

### F8. Flow suggestions misuse the "played" penalty, and the reference track is built in two places
**Problem.**
- `flow.rs:772-784` passes the tracks already in the Flow as the `played` set. That applies the ×0.2 penalty to them, and then they are filtered out anyway. Meanwhile the session's real played set is ignored.
- The suggestion `Reference` is built separately in `app.rs:529-536` and `flow.rs:772`.
- Cache invalidation compares formatted strings (`app.rs:500-505`, `flow.rs:765-770`).

### F9. Search, filter, sort and columns are minimal and hard-coded
**Problem.**
- `Query` knows only free words, one `bpm` range and one `key`.
  - A second `bpm:` replaces the first.
  - An unknown field such as `genre:x` silently becomes a free-text word (`util.rs:505-522`).
- There is no negation, no OR, no quoted phrases and no search within a single field.
- Matching builds a `format!` string for every track on each refilter (`util.rs:544`). On edit that takes a few ms for 50k tracks, which is acceptable, but it would be too slow if it ran every frame.
- There is no sorting. Rows come in path order (`db.rs:170`) and the column headers are static labels (`library_ui.rs:472`).
- The set of columns is fixed (`:452-463`).
- The folder scope is a string-prefix match (`library_ui.rs:198`). The Flow's search popup ignores it (`flow.rs:868-872`).

### F10. Some I/O and computation happens in the frame loop
**Problem.**
- The folder tree calls `list_subdirs` on the UI thread. That also reads each subdirectory to set `has_children` (`library_ui.rs:301`, `library.rs:74-94`).
  - The cache is never invalidated, so it goes stale.
  - It is slow on network drives.
- The header recomputes the `unanalysed` count from the shown rows every frame (`library_ui.rs:388-394`).
- While the folder context menu is open, it scans every track on every frame (`:311-326`).
- `flows::list` loads every card of every Flow to work out `linear` (`flows.rs:317-325`). It runs after every save, and a save happens after every card drag (`app.rs:300-301`).

### F11. Saving is all-or-nothing and there is no undo
**Problem.**
- `flows::save` deletes and re-inserts every card (`flows.rs:450-479`).
- Change detection compares the whole `Vec<Card>` every frame (`flow.rs:217-227`).
- There is no undo for removing a card, for "Reset" (`flow.rs:704`), or for deleting a Flow or folder.

## 3. Target design

### Domain model and query language

```rust
// core/model.rs
pub struct TrackId(pub i64);
pub enum Field { Title, Artist, Album, Genre, Year, Bpm, Key, Energy, Rating, Colour,
                 Duration, DateAdded, PlayCount, LastPlayed, Comment, Tag, Path, Analysed }
pub enum Value { Text(String), Num(f64), Key(Camelot), Bool(bool), Date(i64), None }
pub struct TrackView { id: TrackId, /* resolved fields: user meta ⊕ file tags ⊕ analysis */ }
pub struct Library { tracks: Vec<TrackView>, by_id: HashMap<TrackId,usize>, version: u64 }
impl Library { fn get(&self, f: Field, id) -> Value; fn apply(&mut self, e: &Edit); }

// core/query.rs — one parser, one AST, two backends
pub enum Expr { And(Vec<Expr>), Or(Vec<Expr>), Not(Box<Expr>),
                Text(String), Cmp(Field, Op, Value), Range(Field, Value, Value),
                InCollection(CollectionId), InFolder(PathBuf), Missing }
pub fn parse(s: &str) -> Result<Expr, ParseError>;   // genre:house rating>=4 -tag:vocal "deep space"
pub fn eval(e: &Expr, t: &TrackView) -> bool;       // in-memory, used by the UI
pub fn to_sql(e: &Expr) -> (String, Vec<Value>);    // smart playlists / export
pub struct Sort(Vec<(Field, Dir)>);
```

- **Interactive list:** evaluate in memory. Over a struct-of-arrays index, 50k tracks take about 1–5 ms.
- **Smart playlists and export:** use SQL, or FTS5 for text.

### Collections

```rust
// core/collections.rs — replaces flow_folders/flows/flow_cards
pub enum CollectionKind { Folder, Playlist, Smart { query: String, sort: Sort, limit: Option<u32> }, Flow }
pub struct Collection { id, parent: Option<CollectionId>, kind, name, colour: Option<u8>, position: f64 }
// collection_items(collection_id, item_id PK, track_id, position REAL)  -- ordered list
// flow_layout(collection_id, item_id, parent_item, x, y)                -- Flow = playlist + graph overlay
```

- A Flow becomes a playlist with an optional graph layer. Its list order is either:
  - a depth-first walk of the graph, or
  - the stored `position`, when the graph is linear.
- Every collection then gets a list view, export, and "Add to…".
- Filesystem folders stay a separate "Explorer" source, not a kind of collection. Rename them so they can't be confused with collection folders.

### Track table, selection, drag and actions

```rust
// ui/track_table.rs — one component for library, playlist, smart list, suggestions, Flow list view
pub struct ColumnDef { id: ColumnId, title: &'static str, field: Option<Field>, width: Column,
                       cell: fn(&mut Ui, &TrackView, &RowCtx), sortable: bool }
pub struct TableState { columns: Vec<(ColumnId, f32)>, sort: Sort }      // persisted per view
pub struct TrackTable<'a> { rows: &'a [TrackId], extra: Option<&'a dyn Fn(TrackId)->RowExtra> /* score bars */ }
pub struct Selection { ids: IndexSet<TrackId>, focus: Option<TrackId>, anchor: Option<TrackId> } // ids, not row indices
pub enum DragPayload { Tracks { ids: Vec<TrackId>, origin: Origin }, Collection(CollectionId), Card(CardId) }

// ui/track_actions.rs — one context menu and one action enum
pub enum TrackAction { Load(Option<Deck>), Preview, AddTo(CollectionId), NewPlaylistFrom,
                       Analyse{dynamic: bool}, SetRating(u8), SetColour(u8), AddTag(String),
                       Edit, RevealInExplorer, Relocate, RemoveFrom(CollectionId) }
pub fn track_menu(ui: &mut Ui, sel: &[TrackId], ctx: &MenuCtx) -> Option<TrackAction>;
```

- The app holds one `focused_pane: PaneId` (Library, Collection(id), Suggest or Flow) and one `Selection` per pane.
- `ui_control` and `Source::Selected` read the selection of the active pane.

### Edits and undo

```rust
// core/edit.rs — metadata edits, bulk edits and undo
pub enum Edit { SetField { ids: Vec<TrackId>, field: Field, new: Value, old: Vec<Value> },
                AddToCollection{..}, RemoveFromCollection{..}, MoveItems{..}, FlowGraph(GraphOp) }
pub struct History { undo: Vec<Edit>, redo: Vec<Edit> }  // Edit::inverse(); applied via DB thread, then Library::apply
```

- Edits are written to `track_meta`, so a rescan never overwrites them.
- Writing tags back to files is a separate, explicit job. cratelib could own it.
- Flow graph operations (`GraphOp`: add, remove, link, move) move to `core/flow_graph.rs`. Each operation is saved on its own, instead of deleting and re-inserting everything.

## 4. Issues for the other areas

- **DB.**
  - `Db = Arc<Mutex<Connection>>` is locked from the UI thread for Flow saves and lists (`app.rs:279, 290`).
  - The design says writes go through a single DB thread. Edits and undo need a command queue.
- **Schema.**
  - The design's `tracks`, `crates`, `tags`, `cues`, `sets` and `external_links` tables were never created.
  - Nothing is deployed, so rewriting the migrations from scratch costs less than adding more `ALTER`s.
- **Session history.** `played` exists only in the UI (`library_ui.rs:121`, `app.rs:450`). It should feed the `sets` / `set_plays` tables, play counts and the suggestions engine.
- **Analysis.**
  - Status arrives as strings (`"queued"`, `"running"`, `app.rs:722`).
  - Whether a track is analysed is inferred from grid-source strings (`library_ui.rs:18`).
  - An enum from core would serve both the library status column and smart-playlist queries such as `analysed:no`.
- **Suggestions.**
  - Building the `Reference` should be a single function in core.
  - "Exclude these tracks" should be a separate input from "played" (`flow.rs:778`).
- **Decks.** `deck.rs:23, 77` imports the UI's `library_ui::Track` type. Decks should depend on a `TrackView` from core.
- **Persistence.**
  - Flow UI state is split between eframe storage (scene, open Flow, legacy nodes; `flow.rs:53-63`) and SQLite.
  - Per-collection view state (columns, sort, Flow viewport) should live in one place.
