//! The metadata editor through its window: opened from the track menu's "Edit info…",
//! a typed title saved and undone with Ctrl+Z, "edited" and Revert back to the file's
//! value, and a bulk edit that leaves the mixed fields alone.

use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::core::edit::{EditCmd, Field, Value};
use crate::core::view::Modal;

/// Step until the editor shows where its values came from (they come from the
/// Store just after it opens).
fn sources_shown(h: &mut H, label: &str) {
    for _ in 0..300 {
        if h.query_all_by_label(label).next().is_some() {
            return;
        }
        steps(h, 1);
    }
    panic!("no \"{label}\" in the editor");
}

/// The editor's text box holding `value`.
fn text_box<'a>(h: &'a H, value: &'a str) -> egui_kittest::Node<'a> {
    h.get_all_by_value(value)
        .find(|n| n.accesskit_node().role() == egui::accesskit::Role::TextInput)
        .expect("the text box")
}

#[test]
fn edit_info_saves_undoes_and_reverts() {
    let mut h = start("edit-info");
    let ids = ids(&h);
    h.state_mut().dispatch(ViewCmd::Aside(None).into());
    steps(&mut h, 2);
    let file_title = title(&h, ids[0]);
    let title_now = |a: &EarshotApp| a.index.get(ids[0]).unwrap().title.clone();

    // Right-click the row, "Edit info…".
    h.get_by_label(&file_title).click_secondary();
    steps(&mut h, 2);
    h.get_by_label("Edit info…").click();
    steps(&mut h, 2);
    assert!(h.state().modals.is_open(Modal::Metadata));
    sources_shown(&mut h, "file");

    // Type a new title over the old one and save: the list has it at once.
    text_box(&h, &file_title).focus();
    steps(&mut h, 1);
    press(&mut h, egui::Modifiers::COMMAND, Key::A);
    text_box(&h, &file_title).type_text("Renamed");
    steps(&mut h, 2);
    h.get_by_label("Save").click();
    settle_until(&mut h, |a| title_now(a) == "Renamed");
    assert!(!h.state().modals.is_open(Modal::Metadata));
    h.get_by_label("Renamed");

    // Ctrl+Z undoes it, Ctrl+Shift+Z redoes it.
    press(&mut h, egui::Modifiers::COMMAND, Key::Z);
    settle_until(&mut h, |a| title_now(a) == file_title);
    press(
        &mut h,
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        Key::Z,
    );
    settle_until(&mut h, |a| title_now(a) == "Renamed");

    // Opened again, the title shows as edited; Revert and save: the file's again.
    h.state_mut().dispatch(EditCmd::Editor(vec![ids[0]]).into());
    sources_shown(&mut h, "edited");
    render(&mut h, "edit-info");
    h.get_by_label("Revert").click();
    steps(&mut h, 2);
    h.get_by_label("Save").click();
    settle_until(&mut h, |a| title_now(a) == file_title);

    // Two tracks, one with an artist of its own: rate both, and the artists stay
    // as they were.
    h.state_mut().dispatch(
        EditCmd::Set {
            tracks: vec![ids[1]],
            values: vec![(Field::Artist, Some(Value::Text("Someone".into())))],
        }
        .into(),
    );
    settle_until(&mut h, |a| a.index.get(ids[1]).unwrap().artist == "Someone");
    h.state_mut()
        .dispatch(EditCmd::Editor(vec![ids[1], ids[2]]).into());
    sources_shown(&mut h, "edited");
    h.get_by_label("2 tracks");
    render(&mut h, "edit-info-bulk");
    // The rating's "4", on the Rating row.
    let row = h.get_by_label("Rating").rect().center().y;
    h.get_all_by_label("4")
        .find(|n| (n.rect().center().y - row).abs() < 8.0)
        .expect("the rating's 4")
        .click();
    steps(&mut h, 2);
    h.get_by_label("Save").click();
    let t = |a: &EarshotApp, i: usize| a.index.get(ids[i]).unwrap().clone();
    settle_until(&mut h, |a| {
        t(a, 1).rating == Some(4) && t(a, 2).rating == Some(4)
    });
    assert_eq!(
        (
            t(h.state(), 1).artist.as_str(),
            t(h.state(), 2).artist.as_str()
        ),
        ("Someone", "")
    );
    // One undo step for both.
    h.state_mut().dispatch(Command::Undo);
    settle_until(&mut h, |a| {
        t(a, 1).rating.is_none() && t(a, 2).rating.is_none()
    });
}
