//! Edits that can be undone (`core::edit`): the writer reads what an [`EditCmd`] is
//! about to replace, makes the change, and reports it as an [`Edit`]
//! (`StoreEvent::Edited`). Undo and redo apply an [`Edit`] as it is.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use super::{FLOW_SPACING, Writer, activate_grid, put_meta, reindex, write_graph};
use crate::core::collections::{CollectionKind, FlowGraph, Section};
use crate::core::edit::{self, Edit, EditCmd, Field, FieldChange, SavedCollection, Value};
use crate::core::store::{Result, StoreError, StoreEvent, now, read};

impl Writer {
    /// Make an edit the user asked for, and report it (with what it replaced) as
    /// `StoreEvent::Edited`, for the undo history. An edit that changes nothing isn't
    /// reported.
    pub fn edit(&mut self, cmd: EditCmd) -> Result<Option<Edit>> {
        let e = self.plan(cmd)?;
        if e.is_noop() {
            return Ok(None);
        }
        self.apply_edit(&e)?;
        self.emit(StoreEvent::Edited(e.clone()));
        Ok(Some(e))
    }

    /// The edit `cmd` makes, from what's there now.
    fn plan(&self, cmd: EditCmd) -> Result<Edit> {
        let c = &self.conn;
        let fields = |tracks: &[i64],
                      f: &dyn Fn(i64, Field, Option<Value>) -> Option<Value>,
                      which: &[Field]|
         -> Result<Edit> {
            let mut changes = vec![];
            for &track in tracks {
                for &field in which {
                    let old = user_value(c, track, field)?;
                    let new = f(track, field, old.clone());
                    changes.push(FieldChange {
                        track,
                        field,
                        old,
                        new,
                    });
                }
            }
            Ok(Edit::Fields(changes))
        };
        Ok(match cmd {
            EditCmd::Editor(_) => return Err(StoreError::Refused("not an edit")),
            EditCmd::Set { tracks, values } => {
                let which: Vec<Field> = values.iter().map(|v| v.0).collect();
                let value = |_: i64, field: Field, _: Option<Value>| {
                    values
                        .iter()
                        .find(|v| v.0 == field)
                        .and_then(|v| v.1.clone())
                };
                fields(&tracks, &value, &which)?
            }
            EditCmd::AddTag { tracks, tag } => {
                let add = |_: i64, _: Field, old: Option<Value>| {
                    let mut t = match old {
                        Some(Value::Tags(t)) => t,
                        _ => vec![],
                    };
                    t.push(tag.clone());
                    Some(Value::tags(t))
                };
                fields(&tracks, &add, &[Field::Tags])?
            }
            EditCmd::RemoveTag { tracks, tag } => {
                let remove = |_: i64, _: Field, old: Option<Value>| match old {
                    Some(Value::Tags(t)) => {
                        let t: Vec<String> = t.into_iter().filter(|n| *n != tag).collect();
                        (!t.is_empty()).then_some(Value::Tags(t))
                    }
                    other => other,
                };
                fields(&tracks, &remove, &[Field::Tags])?
            }
            EditCmd::AddTracks { to, tracks } => match read::kind(c, to)? {
                Some(CollectionKind::Crate) => {
                    let old = read::collection_tracks(c, to)?;
                    let mut new = old.clone();
                    for t in tracks {
                        if !new.contains(&t) {
                            new.push(t);
                        }
                    }
                    Edit::Tracks {
                        collection: to,
                        old,
                        new,
                    }
                }
                Some(CollectionKind::Playlist) => {
                    let old = read::collection_tracks(c, to)?;
                    let new = [old.as_slice(), &tracks].concat();
                    Edit::Tracks {
                        collection: to,
                        old,
                        new,
                    }
                }
                Some(CollectionKind::Flow) => {
                    let before = read::flow_graph(c, to)?;
                    let mut after = before.clone();
                    for t in tracks {
                        after.add_root(t, FLOW_SPACING);
                    }
                    edit::flow_edit(to, &before, &after)
                }
                Some(_) => return Err(StoreError::Refused("tracks can't be added to that")),
                None => return Err(StoreError::NoCollection(to)),
            },
            EditCmd::RemoveTracks { from, tracks } => match read::kind(c, from)? {
                Some(CollectionKind::Crate | CollectionKind::Playlist) => {
                    let old = read::collection_tracks(c, from)?;
                    let new = old
                        .iter()
                        .copied()
                        .filter(|t| !tracks.contains(t))
                        .collect();
                    Edit::Tracks {
                        collection: from,
                        old,
                        new,
                    }
                }
                Some(CollectionKind::Flow) => {
                    let before = read::flow_graph(c, from)?;
                    let mut after = before.clone();
                    let gone: Vec<_> = after
                        .nodes
                        .iter()
                        .filter(|n| tracks.contains(&n.track))
                        .map(|n| n.id)
                        .collect();
                    for n in gone {
                        after.remove(n);
                    }
                    edit::flow_edit(from, &before, &after)
                }
                Some(_) => return Err(StoreError::Refused("tracks can't be removed from that")),
                None => return Err(StoreError::NoCollection(from)),
            },
            EditCmd::SetTracks { collection, tracks } => match read::kind(c, collection)? {
                Some(CollectionKind::Crate | CollectionKind::Playlist) => Edit::Tracks {
                    collection,
                    old: read::collection_tracks(c, collection)?,
                    new: tracks,
                },
                Some(_) => {
                    return Err(StoreError::Refused(
                        "only crates and playlists have a track list",
                    ));
                }
                None => return Err(StoreError::NoCollection(collection)),
            },
            EditCmd::Move { collection, parent } => Edit::Parent {
                collection,
                old: collection_row(c, collection)?.0,
                new: parent,
            },
            EditCmd::Rename { collection, name } => Edit::Name {
                collection,
                old: collection_row(c, collection)?.1,
                new: name,
            },
            EditCmd::Create {
                id,
                parent,
                kind,
                section,
                name,
                query,
                tracks,
            } => {
                let section = match (kind.section(), section, parent) {
                    (Some(s), _, _) | (None, Some(s), _) => s,
                    (None, None, Some(p)) => {
                        read::kind_and_section(c, p)?
                            .ok_or(StoreError::NoCollection(p))?
                            .1
                    }
                    (None, None, None) => Section::Playlists,
                };
                let (tracks, graph) = match kind {
                    CollectionKind::Crate => {
                        let mut once = vec![];
                        for t in tracks {
                            if !once.contains(&t) {
                                once.push(t);
                            }
                        }
                        (once, None)
                    }
                    CollectionKind::Playlist => (tracks, None),
                    CollectionKind::Flow => {
                        let mut g = FlowGraph::default();
                        for t in tracks {
                            g.add_root(t, FLOW_SPACING);
                        }
                        (vec![], Some(g))
                    }
                    _ => (vec![], None),
                };
                Edit::Collections {
                    saved: vec![SavedCollection {
                        id,
                        parent,
                        kind,
                        section,
                        name,
                        query,
                        source_flow: None,
                        tracks,
                        graph,
                    }],
                    exists: true,
                }
            }
            EditCmd::Duplicate { from, to } => {
                let mut copy = saved(c, from)?;
                if copy.kind == CollectionKind::Folder {
                    return Err(StoreError::Refused("folders can't be duplicated"));
                }
                copy.id = to;
                copy.name.push_str(" copy");
                Edit::Collections {
                    saved: vec![copy],
                    exists: true,
                }
            }
            EditCmd::Delete(id) => {
                // The collection, then (for a folder) what's in it, each after its
                // folder, so undo can make them in this order.
                let mut all = vec![saved(c, id)?];
                let mut i = 0;
                while i < all.len() {
                    let kids: Vec<i64> = c
                        .prepare_cached(
                            "SELECT id FROM collections WHERE parent_id = ?1 ORDER BY id",
                        )?
                        .query_map([all[i].id], |r| r.get(0))?
                        .collect::<rusqlite::Result<_>>()?;
                    for k in kids {
                        all.push(saved(c, k)?);
                    }
                    i += 1;
                }
                Edit::Collections {
                    saved: all,
                    exists: false,
                }
            }
            EditCmd::RemoveFromLibrary(tracks) => {
                // Only those in it now, so undo puts back just those.
                let mut st =
                    c.prepare_cached("SELECT 1 FROM tracks WHERE id = ?1 AND deleted_at IS NULL")?;
                let mut kept = vec![];
                for t in tracks {
                    if st.exists([t])? && !kept.contains(&t) {
                        kept.push(t);
                    }
                }
                Edit::Removal {
                    tracks: kept,
                    removed: true,
                }
            }
        })
    }

    /// Make an edit as it is (undo applies an inverse, redo the edit again). The
    /// usual events follow: `TracksChanged`, `Collections`.
    pub fn apply_edit(&mut self, e: &Edit) -> Result<()> {
        match e {
            Edit::Fields(changes) => {
                let tx = self.conn.transaction()?;
                let t = now();
                let mut tracks: Vec<i64> = vec![];
                for ch in changes {
                    write_value(&tx, ch.track, ch.field, ch.new.as_ref(), t)?;
                    if !tracks.contains(&ch.track) {
                        tracks.push(ch.track);
                    }
                }
                for &track in &tracks {
                    reindex(&tx, track)?;
                }
                tx.commit()?;
                self.tracks_changed(&tracks)
            }
            Edit::Grid { track, new, .. } => {
                let tx = self.conn.transaction()?;
                match new {
                    Some(grid) => {
                        let bpm: f64 = tx
                            .query_row(
                                "SELECT bpm FROM beatgrids WHERE id = ?1 AND track_id = ?2",
                                params![grid, track],
                                |r| r.get(0),
                            )
                            .optional()?
                            .ok_or(StoreError::Refused("that grid is gone"))?;
                        activate_grid(&tx, *track, *grid, bpm)?;
                    }
                    None => {
                        tx.execute(
                            "UPDATE tracks SET active_grid_id = NULL, bpm = NULL WHERE id = ?1",
                            [track],
                        )?;
                    }
                }
                tx.commit()?;
                self.tracks_changed(&[*track])
            }
            Edit::Tracks {
                collection, new, ..
            } => {
                let tx = self.conn.transaction()?;
                let t = now();
                match read::kind(&tx, *collection)? {
                    Some(CollectionKind::Crate) => {
                        let keep: HashSet<i64> = new.iter().copied().collect();
                        for old in read::collection_tracks(&tx, *collection)? {
                            if !keep.contains(&old) {
                                tx.execute(
                                    "DELETE FROM crate_tracks WHERE collection_id = ?1 AND track_id = ?2",
                                    params![collection, old],
                                )?;
                            }
                        }
                        for track in new {
                            tx.execute(
                                "INSERT OR IGNORE INTO crate_tracks(collection_id, track_id, added_at)
                                 VALUES (?1, ?2, ?3)",
                                params![collection, track, t],
                            )?;
                        }
                    }
                    Some(CollectionKind::Playlist) => {
                        tx.execute(
                            "DELETE FROM playlist_entries WHERE collection_id = ?1",
                            [collection],
                        )?;
                        for (i, track) in new.iter().enumerate() {
                            tx.execute(
                                "INSERT INTO playlist_entries(collection_id, position, track_id)
                                 VALUES (?1, ?2, ?3)",
                                params![collection, i as i64, track],
                            )?;
                        }
                    }
                    Some(_) => {
                        return Err(StoreError::Refused(
                            "only crates and playlists have a track list",
                        ));
                    }
                    None => return Err(StoreError::NoCollection(*collection)),
                }
                tx.execute(
                    "UPDATE collections SET updated_at = ?2 WHERE id = ?1",
                    params![collection, t],
                )?;
                tx.commit()?;
                self.emit_collections()
            }
            Edit::Parent {
                collection, new, ..
            } => self.move_to(*collection, *new),
            Edit::Name {
                collection, new, ..
            } => self.rename(*collection, new),
            Edit::Collections {
                saved,
                exists: true,
            } => {
                // Each after its folder.
                for s in saved {
                    self.check_folder(s.parent, s.section)
                        .or_else(|e| match e {
                            // The folder is in this edit, made just before.
                            StoreError::NoCollection(p) if saved.iter().any(|o| o.id == p) => {
                                Ok(())
                            }
                            e => Err(e),
                        })?;
                }
                let tx = self.conn.transaction()?;
                let t = now();
                for s in saved {
                    tx.execute(
                        "INSERT INTO collections(id, parent_id, kind, section, name, query,
                           source_flow_id, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                        params![
                            s.id,
                            s.parent,
                            s.kind.as_str(),
                            s.section.as_str(),
                            s.name,
                            s.query,
                            s.source_flow,
                            t
                        ],
                    )?;
                    for (i, track) in s.tracks.iter().enumerate() {
                        match s.kind {
                            CollectionKind::Crate => tx.execute(
                                "INSERT OR IGNORE INTO crate_tracks(collection_id, track_id, added_at)
                                 VALUES (?1, ?2, ?3)",
                                params![s.id, track, t],
                            )?,
                            _ => tx.execute(
                                "INSERT INTO playlist_entries(collection_id, position, track_id)
                                 VALUES (?1, ?2, ?3)",
                                params![s.id, i as i64, track],
                            )?,
                        };
                    }
                    if let Some(g) = &s.graph {
                        write_graph(&tx, s.id, g)?;
                    }
                }
                tx.commit()?;
                self.emit_collections()
            }
            Edit::Collections {
                saved,
                exists: false,
            } => {
                let tx = self.conn.transaction()?;
                // What's inside first; deleting a folder takes the rest with it anyway.
                for s in saved.iter().rev() {
                    tx.execute("DELETE FROM collections WHERE id = ?1", [s.id])?;
                }
                tx.commit()?;
                self.emit(StoreEvent::Deleted(saved.iter().map(|s| s.id).collect()));
                self.emit_collections()
            }
            Edit::Removal { tracks, removed } => {
                let tx = self.conn.transaction()?;
                let at = removed.then(now);
                for t in tracks {
                    tx.execute(
                        "UPDATE tracks SET deleted_at = ?2 WHERE id = ?1",
                        params![t, at],
                    )?;
                    // Its queued analysis goes with it (`Analyser::forget` drops it
                    // from the running queue); putting it back doesn't queue it again.
                    if *removed {
                        tx.execute(
                            "DELETE FROM jobs WHERE track_id = ?1 AND kind LIKE ?2 || '%'",
                            params![t, read::JOB_PREFIX],
                        )?;
                    }
                }
                tx.commit()?;
                self.emit_library()
            }
            Edit::FlowGraph { flow, ops, .. } => {
                let tx = self.conn.transaction()?;
                if read::kind(&tx, *flow)? != Some(CollectionKind::Flow) {
                    return Err(StoreError::NoCollection(*flow));
                }
                let mut graph = read::flow_graph(&tx, *flow)?;
                for op in ops {
                    graph
                        .apply(op, FLOW_SPACING)
                        .map_err(|_| StoreError::Refused("the Flow has changed since"))?;
                }
                write_graph(&tx, *flow, &graph)?;
                tx.commit()?;
                self.emit_collections()
            }
        }
    }
}

/// A collection as it is now, with what's in it.
fn saved(c: &Connection, id: i64) -> Result<SavedCollection> {
    let (parent, kind, section, name, query, source_flow): (
        Option<i64>,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
    ) = c
        .query_row(
            "SELECT parent_id, kind, section, name, query, source_flow_id
             FROM collections WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(StoreError::NoCollection(id))?;
    let kind = CollectionKind::parse(&kind).ok_or(StoreError::NoCollection(id))?;
    let section = Section::parse(&section).ok_or(StoreError::NoCollection(id))?;
    let tracks = match kind {
        CollectionKind::Crate | CollectionKind::Playlist => read::collection_tracks(c, id)?,
        _ => vec![],
    };
    let graph = match kind {
        CollectionKind::Flow => Some(read::flow_graph(c, id)?),
        _ => None,
    };
    Ok(SavedCollection {
        id,
        parent,
        kind,
        section,
        name,
        query,
        source_flow,
        tracks,
        graph,
    })
}

/// (parent, name) of a collection.
fn collection_row(c: &Connection, id: i64) -> Result<(Option<i64>, String)> {
    c.query_row(
        "SELECT parent_id, name FROM collections WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or(StoreError::NoCollection(id))
}

/// What the user has set for a field (not the file's tag): what an edit replaces.
fn user_value(c: &Connection, track: i64, field: Field) -> Result<Option<Value>> {
    if let Some(name) = field.meta() {
        let v: Option<String> = c
            .prepare_cached(
                "SELECT value FROM track_meta WHERE track_id = ?1 AND field = ?2 AND source = 'user'",
            )?
            .query_row(params![track, name], |r| r.get(0))
            .optional()?;
        return Ok(v.map(Value::Text));
    }
    Ok(match field {
        Field::Rating | Field::Colour => {
            let col = if field == Field::Rating {
                "rating"
            } else {
                "color"
            };
            let v: Option<i64> = c
                .query_row(
                    &format!("SELECT {col} FROM tracks WHERE id = ?1"),
                    [track],
                    |r| r.get(0),
                )
                .optional()?
                .ok_or(StoreError::NoTrack(track))?;
            match field {
                Field::Rating => v.and_then(|v| u8::try_from(v).ok()).map(Value::Rating),
                _ => v.and_then(|v| u32::try_from(v).ok()).map(Value::Colour),
            }
        }
        _ => {
            let names: Vec<String> = c
                .prepare_cached(
                    "SELECT g.name FROM track_tags tt JOIN tags g ON g.id = tt.tag_id
                     WHERE tt.track_id = ?1 ORDER BY g.name",
                )?
                .query_map([track], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            (!names.is_empty()).then_some(Value::Tags(names))
        }
    })
}

/// Write the user's value for a field (`None` clears it).
fn write_value(c: &Connection, track: i64, field: Field, v: Option<&Value>, t: i64) -> Result<()> {
    let wrong = || StoreError::Refused("a value of the wrong kind for that field");
    if let Some(name) = field.meta() {
        let text = match v {
            Some(Value::Text(s)) => Some(s.trim()),
            None => None,
            Some(_) => return Err(wrong()),
        };
        return put_meta(c, track, name, "user", text, t);
    }
    match field {
        Field::Rating | Field::Colour => {
            let n: Option<i64> = match (field, v) {
                (_, None) => None,
                (Field::Rating, Some(Value::Rating(r))) => Some((*r).min(5) as i64),
                (Field::Colour, Some(Value::Colour(rgb))) => Some((*rgb & 0xFF_FFFF) as i64),
                _ => return Err(wrong()),
            };
            let col = if field == Field::Rating {
                "rating"
            } else {
                "color"
            };
            let changed = c.execute(
                &format!("UPDATE tracks SET {col} = ?2 WHERE id = ?1"),
                params![track, n],
            )?;
            if changed == 0 {
                return Err(StoreError::NoTrack(track));
            }
        }
        _ => {
            let names: &[String] = match v {
                Some(Value::Tags(t)) => t,
                None => &[],
                Some(_) => return Err(wrong()),
            };
            c.execute("DELETE FROM track_tags WHERE track_id = ?1", [track])?;
            for name in names {
                c.prepare_cached("INSERT OR IGNORE INTO tags(category, name) VALUES ('', ?1)")?
                    .execute([name])?;
                c.prepare_cached(
                    "INSERT OR IGNORE INTO track_tags(track_id, tag_id, source)
                     SELECT ?1, id, 'user' FROM tags WHERE category = '' AND name = ?2",
                )?
                .execute(params![track, name])?;
            }
        }
    }
    Ok(())
}
