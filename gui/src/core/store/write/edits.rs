//! Edits that can be undone (`core::edit`): the writer reads what an [`EditCmd`] is
//! about to replace, makes the change, and reports it as an [`Edit`]
//! (`StoreEvent::Edited`). Undo and redo apply an [`Edit`] as it is.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use super::{FLOW_SPACING, Writer, activate_grid, put_meta, reindex, write_graph};
use crate::core::collections::CollectionKind;
use crate::core::edit::{self, Edit, EditCmd, Field, FieldChange, Value};
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
