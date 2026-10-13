//! The History (All tracks > Library > History): a session's plays in order in the
//! track table, a new play listed as it counts, the Plays and Last played columns, and
//! "Save as playlist" (undoable).

use egui_kittest::kittest::Queryable;

use super::*;
use crate::core::view::Source;
use crate::ui::panels::library::table::{ColumnId, TableView};

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Play `id` on deck A and count it as played (as if it started 31 s ago).
fn play(h: &mut H, id: i64) {
    let deck = DeckId::new(0).unwrap();
    h.state_mut().load(Some(deck), id);
    settle_until(h, |a| {
        a.panels.decks[0].wave.is_some() && a.deck_now(deck).track == Some(id)
    });
    h.state_mut()
        .dispatch((deck, EngineCmd::Play(earshot_engine::Set::On)).into());
    settle_until(h, |a| a.deck_now(deck).playing);
    let decks = h.state().decks_now();
    let ago = Instant::now() - crate::core::history::PLAYED_AFTER - Duration::from_secs(1);
    let stopped: Vec<DeckNow> = decks
        .iter()
        .map(|d| DeckNow {
            playing: false,
            ..*d
        })
        .collect();
    h.state_mut().history.tick(ago, &stopped);
    h.state_mut().history.tick(ago, &decks);
    h.state_mut().track_plays();
}

#[test]
fn history_lists_a_session_in_play_order() {
    let mut h = start("history");
    let ids = ids(&h);
    h.state_mut().dispatch(ViewCmd::Aside(None).into());
    // Three plays in this run's session, one track twice.
    let store = h.state().lib.store.clone();
    let now = unix_now();
    for (track, ago) in [(ids[2], 900), (ids[0], 600), (ids[2], 300)] {
        store.send(StoreCmd::SavePlay {
            track,
            deck: 0,
            started_at: now - ago,
        });
    }
    // The Plays column's count comes with the track.
    settle_until(&mut h, |a| a.index.get(ids[2]).unwrap().plays == 2);
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Source::AllTracks)).into());
    steps(&mut h, 2);
    h.get_by_label("History").click();
    let shown = |a: &EarshotApp| a.panels.library.browser.shown().to_vec();
    settle_until(&mut h, |a| shown(a) == [ids[2], ids[0], ids[2]]);
    h.get_by_label("3 plays, in the order they played.");
    {
        let t = h
            .state_mut()
            .panels
            .library
            .tables
            .get_mut(TableView::History);
        t.toggle_column(ColumnId::Plays);
        t.toggle_column(ColumnId::LastPlayed);
    }
    steps(&mut h, 3);
    h.get_all_by_label("5 minutes ago")
        .next()
        .expect("last played");

    // A play that counts now is listed at once (and the newest session is the one
    // shown).
    play(&mut h, ids[1]);
    settle_until(&mut h, |a| shown(a).len() == 4);
    assert_eq!(shown(h.state())[3], ids[1]);
    assert_eq!(h.state().index.get(ids[1]).unwrap().plays, 1);
    steps(&mut h, 3);
    render(&mut h, "history");

    // Sorted by plays, most first.
    h.state_mut()
        .panels
        .library
        .tables
        .get_mut(TableView::History)
        .sort = Some((ColumnId::Plays, crate::core::query::Dir::Desc));
    steps(&mut h, 2);
    assert_eq!(&shown(h.state())[..2], [ids[2], ids[2]]);

    // Saved as a playlist, in play order; undo takes it away.
    h.get_by_label("Save as playlist").click();
    let saved = |a: &EarshotApp| {
        a.collections
            .children(None)
            .into_iter()
            .find(|c| c.kind == CollectionKind::Playlist && c.name.starts_with("Played "))
            .map(|c| c.tracks.clone())
    };
    settle_until(&mut h, |a| saved(a).is_some());
    assert_eq!(saved(h.state()).unwrap(), [ids[2], ids[0], ids[2], ids[1]]);
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| saved(a).is_none());
}
