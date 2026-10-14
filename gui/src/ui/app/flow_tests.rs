//! Prepare's Flow tab, driven through the whole app: renaming from the canvas,
//! dragging cards, dropping tracks, the suggestions pane, and what's kept in the
//! database between runs.

use egui::{PointerButton, Pos2, pos2, vec2};
use egui_kittest::kittest::Queryable;

use super::tests::{H, ids, render, settle_until, start, steps};
use super::*;
use crate::core::view::Tool;
use crate::core::worker_client::testing::stand_in;
use crate::ui::panels::library::drag::{self, DragPayload, Origin};

/// The app in Prepare's Flow tab.
fn flow_tab(test: &str) -> H {
    let mut h = start(test);
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(ViewCmd::Tool(Tool::Flow).into());
    steps(&mut h, 3);
    h
}

/// Put `n` tracks on the canvas as cards, and wait for the Flow to be saved.
fn with_cards(h: &mut H, n: usize) -> CollectionId {
    let ids: Vec<i64> = ids(h).into_iter().take(n).collect();
    h.state_mut().dispatch(FlowCmd::AddTracks(ids).into());
    h.state_mut().flow().fit();
    settle_until(h, |a| {
        a.panels
            .flow
            .flow
            .is_some_and(|id| a.collections.get(id).is_some())
    });
    steps(h, 5);
    h.state().panels.flow.flow.unwrap()
}

fn press(h: &H, at: Pos2, pressed: bool) {
    h.event(egui::Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    });
}

/// Move the pointer from `from` to `to` over a few frames, as a hand would.
fn glide(h: &mut H, from: Pos2, to: Pos2) {
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        steps(h, 1);
    }
}

#[test]
fn the_title_renames_the_flow_as_it_is_typed() {
    let mut h = flow_tab("flow-title");
    let id = with_cards(&mut h, 1);
    let name = CollectionKind::Flow.untitled();
    // The title field (the tree's row has the name too).
    let field = |n: &egui_kittest::kittest::AccessKitNode<'_>| {
        n.role() == egui::accesskit::Role::TextInput && n.value().as_deref() == Some(name)
    };
    h.get_by(field).focus();
    steps(&mut h, 2);
    h.key_press(egui::Key::End);
    steps(&mut h, 1);
    h.get_by(field).type_text(" Night");
    // No Enter: the tree follows as it's typed.
    settle_until(&mut h, |a| {
        a.collections
            .get(id)
            .is_some_and(|c| c.name == "Untitled Flow Night")
    });
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(crate::core::view::Source::Flows)).into());
    steps(&mut h, 3);
    assert!(
        h.query_by_label_contains("Untitled Flow Night").is_some(),
        "the library's tree shows the new name"
    );
    assert_eq!(h.state().panels.flow.name, "Untitled Flow Night");
}

#[test]
fn a_card_dragged_off_the_canvas_has_a_ghost_and_stays_put() {
    let mut h = flow_tab("flow-ghost");
    with_cards(&mut h, 1);
    let node = h.state().panels.flow.graph().nodes[0].clone();
    let at = h.state().panels.flow.card_on_screen(node.id).unwrap();

    // Moved on the canvas: it moves.
    h.hover_at(at);
    steps(&mut h, 1);
    press(&h, at, true);
    steps(&mut h, 1);
    let to = at + vec2(60.0, 30.0);
    glide(&mut h, at, to);
    assert!(!h.state().panels.flow.dragged_off());
    assert!(drag::payload(&h.ctx).is_none(), "no ghost on the canvas");
    press(&h, to, false);
    steps(&mut h, 2);
    let moved = h.state().panels.flow.graph().nodes[0].clone();
    assert!(
        moved.x > node.x + 20.0 && moved.y > node.y + 10.0,
        "{moved:?}"
    );

    // Dragged over the library: a ghost follows the pointer and the card stays put.
    let at = h.state().panels.flow.card_on_screen(node.id).unwrap();
    h.hover_at(at);
    steps(&mut h, 1);
    press(&h, at, true);
    steps(&mut h, 1);
    let library = pos2(250.0, at.y);
    glide(&mut h, at, library);
    assert!(h.state().panels.flow.dragged_off());
    assert!(
        matches!(drag::payload(&h.ctx).as_deref(), Some(DragPayload::Tracks { ids, origin: Origin::Flow }) if *ids == vec![node.track]),
        "it carries its track, so the shell draws the ghost"
    );
    render(&mut h, "flow-ghost");
    press(&h, library, false);
    steps(&mut h, 2);
    assert!(!h.state().panels.flow.dragged_off());
    assert!(drag::payload(&h.ctx).is_none());
    assert_eq!(h.state().panels.flow.graph().nodes[0], moved);
}

#[test]
fn dropping_tracks_on_the_canvas_adds_a_card_each() {
    let mut h = flow_tab("flow-drop");
    let tracks: Vec<i64> = ids(&h).into_iter().take(3).collect();
    let at = h.state().panels.flow.card_on_screen_at(pos2(300.0, 150.0));
    h.hover_at(at);
    steps(&mut h, 1);
    press(&h, at, true);
    steps(&mut h, 1);
    // As if dragged from the library: several tracks at once.
    egui::DragAndDrop::set_payload(
        &h.ctx,
        DragPayload::Tracks {
            ids: tracks.clone(),
            origin: Origin::Library,
        },
    );
    glide(&mut h, at, at + vec2(10.0, 10.0));
    press(&h, at + vec2(10.0, 10.0), false);
    steps(&mut h, 3);
    let g = h.state().panels.flow.graph();
    let on: Vec<i64> = g.nodes.iter().map(|n| n.track).collect();
    assert_eq!(on, tracks);
    assert!(g.edges.is_empty(), "dropped cards aren't linked");
    settle_until(&mut h, |a| a.panels.flow.flow.is_some());
}

#[test]
fn the_suggestions_pane_opens_and_closes_from_its_icon() {
    let mut h = flow_tab("flow-suggest");
    with_cards(&mut h, 1);
    let tip = "Suggestions: what could follow the selected card";
    assert!(h.query_by_label("After").is_some(), "open at first");
    h.get_by_label(tip).click();
    steps(&mut h, 20);
    assert_eq!(h.state().prefs.prepare.aside, None);
    assert!(h.query_by_label("After").is_none(), "closed");
    h.get_by_label(tip).click();
    steps(&mut h, 20);
    assert!(h.state().prefs.prepare.aside.is_some());
    assert!(h.query_by_label("After").is_some(), "open again");
}

#[test]
fn the_open_flow_and_its_view_come_back_from_the_database() {
    let test = "flow-restore";
    let mut h = flow_tab(test);
    let id = with_cards(&mut h, 2);
    let view = [-100.0, -50.0, 900.0, 450.0];
    h.state_mut().flow().set_view(view);
    let reader = h.state().lib.store.reader.clone();
    let saved = |key: &str| reader.setting(key).ok().flatten();
    settle_until(&mut h, |_| {
        saved(settings::FLOW_OPEN).as_deref() == Some(id.to_string().as_str())
            && reader.flow_camera(id).ok().flatten().is_some()
    });
    // The camera is kept as the view was (the canvas may have resized it to fit).
    let kept = h.state().panels.flow.view();
    drop(h);

    // A second run on the same database.
    let services = Services {
        db: super::tests::scratch_dir(test).join(store::FILE_NAME),
        null_audio: true,
        worker: stand_in,
        midi: false,
    };
    let mut h = egui_kittest::Harness::builder()
        .with_size(vec2(1700.0, 1000.0))
        .build_eframe(move |cc| EarshotApp::new(cc, services).expect("app"));
    settle_until(&mut h, |a| a.panels.flow.flow == Some(id));
    let flow = &h.state().panels.flow;
    assert_eq!(flow.graph().nodes.len(), 2);
    assert_eq!(flow.view(), kept);
}
