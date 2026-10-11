//! Saved Flows, in folders, in `earshot.db`.
//!
//! A Flow is a set of cards (one track each) where a card can follow another. A
//! playlist is just a Flow whose cards form one chain: [`playlist_order`] gives its
//! tracks in order, and is `None` when the Flow branches.

use std::collections::HashMap;

use rusqlite::{Connection, params};

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub id: i64,
    /// `None`: top level.
    pub parent: Option<i64>,
    pub name: String,
}

/// A saved Flow as the folder tree lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowEntry {
    pub id: i64,
    /// `None`: top level.
    pub folder: Option<i64>,
    pub name: String,
    pub cards: usize,
    /// One chain of cards, i.e. a playlist.
    pub linear: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub id: u64,
    pub track_id: i64,
    /// The card this one follows; `None` for a starting card.
    pub parent: Option<u64>,
    pub x: f32,
    pub y: f32,
}

/// The tracks in order if the cards form a single chain (or there are none), else
/// `None`. Links to cards that aren't there count as no link.
pub fn playlist_order(cards: &[Card]) -> Option<Vec<i64>> {
    let by_id: HashMap<u64, &Card> = cards.iter().map(|c| (c.id, c)).collect();
    let parent = |c: &Card| c.parent.filter(|p| by_id.contains_key(p));
    let mut roots = cards.iter().filter(|c| parent(c).is_none());
    let mut next: HashMap<u64, &Card> = HashMap::new();
    for c in cards {
        if let Some(p) = parent(c)
            && next.insert(p, c).is_some()
        {
            return None; // two cards follow one: a branch
        }
    }
    let mut order = vec![];
    if let Some(mut at) = roots.next() {
        if roots.next().is_some() {
            return None; // two starting cards
        }
        loop {
            order.push(at.track_id);
            match next.get(&at.id) {
                Some(n) => at = n,
                None => break,
            }
        }
    }
    (order.len() == cards.len()).then_some(order)
}

// ---- folders and Flows ------------------------------------------------------------

/// Every folder and Flow, sorted by name.
pub fn list(conn: &Connection) -> Result<(Vec<Folder>, Vec<FlowEntry>), String> {
    let mut st = conn
        .prepare("SELECT id, parent_id, name FROM flow_folders ORDER BY name COLLATE NOCASE")
        .map_err(err)?;
    let folders = st
        .query_map([], |r| {
            Ok(Folder {
                id: r.get(0)?,
                parent: r.get(1)?,
                name: r.get(2)?,
            })
        })
        .map_err(err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(err)?;
    let mut st = conn
        .prepare("SELECT id, folder_id, name FROM flows ORDER BY name COLLATE NOCASE")
        .map_err(err)?;
    let rows = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(err)?
        .collect::<Result<Vec<(i64, Option<i64>, String)>, _>>()
        .map_err(err)?;
    let mut flows = vec![];
    for (id, folder, name) in rows {
        let cards = load(conn, id)?;
        flows.push(FlowEntry {
            id,
            folder,
            name,
            cards: cards.len(),
            linear: playlist_order(&cards).is_some(),
        });
    }
    Ok((folders, flows))
}

pub fn create_folder(conn: &Connection, parent: Option<i64>, name: &str) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO flow_folders(parent_id, name) VALUES (?1, ?2)",
        params![parent, name],
    )
    .map_err(err)?;
    Ok(conn.last_insert_rowid())
}

pub fn rename_folder(conn: &Connection, id: i64, name: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE flow_folders SET name = ?2 WHERE id = ?1",
        params![id, name],
    )
    .map_err(err)?;
    Ok(())
}

/// Move a folder into another (or to the top level). Refused if `parent` is inside it.
pub fn move_folder(conn: &Connection, id: i64, parent: Option<i64>) -> Result<(), String> {
    let mut at = parent;
    while let Some(p) = at {
        if p == id {
            return Err("can't move a folder into itself".into());
        }
        at = conn
            .query_row(
                "SELECT parent_id FROM flow_folders WHERE id = ?1",
                [p],
                |r| r.get(0),
            )
            .map_err(err)?;
    }
    conn.execute(
        "UPDATE flow_folders SET parent_id = ?2 WHERE id = ?1",
        params![id, parent],
    )
    .map_err(err)?;
    Ok(())
}

/// Delete a folder with everything in it.
pub fn delete_folder(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM flow_folders WHERE id = ?1", [id])
        .map_err(err)?;
    Ok(())
}

pub fn create(conn: &Connection, folder: Option<i64>, name: &str) -> Result<i64, String> {
    conn.execute(
        "INSERT INTO flows(folder_id, name) VALUES (?1, ?2)",
        params![folder, name],
    )
    .map_err(err)?;
    Ok(conn.last_insert_rowid())
}

pub fn rename(conn: &Connection, id: i64, name: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE flows SET name = ?2 WHERE id = ?1",
        params![id, name],
    )
    .map_err(err)?;
    Ok(())
}

pub fn move_to(conn: &Connection, id: i64, folder: Option<i64>) -> Result<(), String> {
    conn.execute(
        "UPDATE flows SET folder_id = ?2 WHERE id = ?1",
        params![id, folder],
    )
    .map_err(err)?;
    Ok(())
}

pub fn delete(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM flows WHERE id = ?1", [id])
        .map_err(err)?;
    Ok(())
}

/// A copy of a Flow beside it, named "<name> copy". Returns the new id.
pub fn duplicate(conn: &mut Connection, id: i64) -> Result<i64, String> {
    let (folder, name): (Option<i64>, String) = conn
        .query_row(
            "SELECT folder_id, name FROM flows WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(err)?;
    let cards = load(conn, id)?;
    let copy = create(conn, folder, &format!("{name} copy"))?;
    save(conn, copy, &cards)?;
    Ok(copy)
}

// ---- cards ------------------------------------------------------------------------

pub fn load(conn: &Connection, flow: i64) -> Result<Vec<Card>, String> {
    let mut st = conn
        .prepare_cached(
            "SELECT card_id, track_id, parent_card, x, y FROM flow_cards
             WHERE flow_id = ?1 ORDER BY card_id",
        )
        .map_err(err)?;
    st.query_map([flow], |r| {
        Ok(Card {
            id: r.get::<_, i64>(0)? as u64,
            track_id: r.get(1)?,
            parent: r.get::<_, Option<i64>>(2)?.map(|p| p as u64),
            x: r.get::<_, f64>(3)? as f32,
            y: r.get::<_, f64>(4)? as f32,
        })
    })
    .map_err(err)?
    .collect::<Result<_, _>>()
    .map_err(err)
}

/// Replace a Flow's cards.
pub fn save(conn: &mut Connection, flow: i64, cards: &[Card]) -> Result<(), String> {
    let tx = conn.transaction().map_err(err)?;
    tx.execute("DELETE FROM flow_cards WHERE flow_id = ?1", [flow])
        .map_err(err)?;
    {
        let mut st = tx
            .prepare(
                "INSERT INTO flow_cards(flow_id, card_id, track_id, parent_card, x, y)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )
            .map_err(err)?;
        for c in cards {
            st.execute(params![
                flow,
                c.id as i64,
                c.track_id,
                c.parent.map(|p| p as i64),
                c.x as f64,
                c.y as f64
            ])
            .map_err(err)?;
        }
    }
    tx.execute(
        "UPDATE flows SET updated_at = CURRENT_TIMESTAMP WHERE id = ?1",
        [flow],
    )
    .map_err(err)?;
    tx.commit().map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: u64, parent: Option<u64>) -> Card {
        Card {
            id,
            track_id: id as i64 * 10,
            parent,
            x: 0.0,
            y: 0.0,
        }
    }

    #[test]
    fn a_chain_is_a_playlist() {
        let cards = [card(3, Some(1)), card(1, None), card(2, Some(3))];
        assert_eq!(playlist_order(&cards), Some(vec![10, 30, 20]));
        assert_eq!(playlist_order(&[]), Some(vec![]));
        assert_eq!(playlist_order(&[card(1, None)]), Some(vec![10]));
        // A link to a missing card counts as no link.
        assert_eq!(playlist_order(&[card(1, Some(9))]), Some(vec![10]));
    }

    #[test]
    fn branches_and_islands_are_not() {
        let branch = [card(1, None), card(2, Some(1)), card(3, Some(1))];
        assert_eq!(playlist_order(&branch), None);
        let two_starts = [card(1, None), card(2, None)];
        assert_eq!(playlist_order(&two_starts), None);
    }

    fn test_db() -> Connection {
        let dir = std::env::temp_dir().join(format!("earshot-flows-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::core::db::open(&dir.join("earshot.db")).unwrap();
        let conn = std::sync::Arc::try_unwrap(db)
            .ok()
            .unwrap()
            .into_inner()
            .unwrap();
        for id in [10, 20, 30] {
            conn.execute(
                "INSERT INTO tracks(id, path, size, mtime) VALUES (?1, ?2, 0, 0)",
                params![id, format!("/t/{id}.mp3")],
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn save_load_move_and_delete() {
        let mut conn = test_db();
        let sets = create_folder(&conn, None, "Sets").unwrap();
        let summer = create_folder(&conn, Some(sets), "Summer").unwrap();
        assert!(move_folder(&conn, sets, Some(summer)).is_err(), "no loops");

        let f = create(&conn, Some(summer), "Warm-up").unwrap();
        let cards = vec![card(1, None), card(2, Some(1))];
        save(&mut conn, f, &cards).unwrap();
        assert_eq!(load(&conn, f).unwrap(), cards);

        let copy = duplicate(&mut conn, f).unwrap();
        rename(&conn, copy, "Peak").unwrap();
        move_to(&conn, copy, None).unwrap();
        let (folders, flows) = list(&conn).unwrap();
        assert_eq!(folders.len(), 2);
        let peak = flows.iter().find(|e| e.id == copy).unwrap();
        assert_eq!(
            (peak.name.as_str(), peak.folder, peak.cards),
            ("Peak", None, 2)
        );
        assert!(peak.linear);

        // Deleting a folder takes its subfolders and Flows with it; others stay.
        delete_folder(&conn, sets).unwrap();
        let (folders, flows) = list(&conn).unwrap();
        assert!(folders.is_empty());
        assert_eq!(flows.iter().map(|e| e.id).collect::<Vec<_>>(), vec![copy]);
        assert!(load(&conn, f).unwrap().is_empty());
    }
}
