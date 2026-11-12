//! The Tempo lock tab, driven through the window: the track menu opens it, the stand-in
//! worker's chart reaches it, and Straighten refuses a wild BPM.

use egui_kittest::kittest::{NodeT, Queryable};

use super::*;
use crate::core::view::Tool;

/// Right-click `title` in the library and choose "Normalise tempo"; wait for the chart.
fn normalise(h: &mut H, title: &str) {
    h.get_all_by_label(title)
        .next()
        .expect("the track's row")
        .click_secondary();
    steps(h, 3);
    h.get_by_label("Normalise tempo").click();
    settle_until(h, |a| a.panels.tempo_lock.chart.is_some());
    steps(h, 3);
}

fn straighten_button<'a>(h: &'a H) -> egui_kittest::Node<'a> {
    h.get_all_by_label("Straighten")
        .find(|n| n.accesskit_node().role() == egui::accesskit::Role::Button)
        .expect("the Straighten button")
}

#[test]
fn normalise_tempo_opens_the_tab_and_analyses() {
    let mut h = start("tempo-menu");
    let id = h
        .state()
        .index
        .tracks()
        .iter()
        .find(|t| t.title == "Night Bus")
        .unwrap()
        .id;
    normalise(&mut h, "Night Bus");
    let app = h.state();
    assert_eq!(app.prefs.mode, Mode::Prepare);
    assert_eq!(app.prefs.layout().tool, Tool::TempoLock);
    assert_eq!(app.tempo_lock.current().track, Some(id));
    let tab = &app.panels.tempo_lock;
    assert_eq!(tab.track, Some(id));
    let a = &tab.chart.as_ref().unwrap().analysis;
    // The tab offers the rounded median, and shows the stand-in's stats and warning.
    assert_eq!(tab.bpm, a.suggested_bpm);
    assert!(tab.busy.is_none());
    assert_eq!(a.tempo_changes.len(), 1);
    h.get_by_label("MEDIAN TEMPO");
    h.get_by_label("A tempo change that looks deliberate:");
}

#[test]
fn a_track_dragged_onto_the_tab_is_analysed() {
    let mut h = start("tempo-drop");
    let app = h.state_mut();
    app.dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    app.dispatch(ViewCmd::Tool(Tool::TempoLock).into());
    steps(&mut h, 3);
    let id = h
        .state()
        .index
        .tracks()
        .iter()
        .find(|t| t.title == "Low Orbit")
        .unwrap()
        .id;
    let from = at(&h, "Low Orbit").center();
    let to = h
        .get_all_by_label_contains("Drag a track here")
        .next()
        .expect("the hint")
        .rect()
        .center();
    drag(&mut h, from, to, |_| {});
    settle_until(&mut h, |a| a.panels.tempo_lock.chart.is_some());
    assert_eq!(h.state().panels.tempo_lock.track, Some(id));
}

#[test]
fn straighten_is_disabled_for_a_bpm_out_of_range() {
    let mut h = start("tempo-range");
    normalise(&mut h, "Glass Steps");
    assert!(!straighten_button(&h).accesskit_node().is_disabled());
    for wild in [300.0, 10.0] {
        h.state_mut().panels.tempo_lock.bpm = wild;
        steps(&mut h, 2);
        assert!(
            straighten_button(&h).accesskit_node().is_disabled(),
            "{wild} BPM"
        );
    }
    // Half-time: twice the detected tempo is fine.
    let median = h
        .state()
        .panels
        .tempo_lock
        .chart
        .as_ref()
        .unwrap()
        .analysis
        .median_bpm;
    let tab = &mut h.state_mut().panels.tempo_lock;
    tab.level = crate::core::tempo_lock::BeatLevel::HalfTime;
    tab.bpm = (median * 2.0).round();
    steps(&mut h, 2);
    assert!(!straighten_button(&h).accesskit_node().is_disabled());
}

