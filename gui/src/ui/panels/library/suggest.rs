//! The Suggestions pane: what to play after the reference track, ranked, with each
//! part of the score shown as a small bar. Master / A / B / Selected picks the
//! reference (`core::history` works it out; in Prepare, Master is Prepare's player
//! while it plays); Drop / Hold / Build picks the energy direction. The list is the
//! one track table, so its columns, sort and filters work as everywhere else.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use earshot_engine::DeckId;
use egui::{Align, RichText, Ui};

use super::drag::Origin;
use super::table::{ColumnId, TableState, TableView, TrackTable};
use super::tracks::{Media, MenuAt};
use crate::core::bus::deck_letter;
use crate::core::command::Command;
use crate::core::history::{DeckNow, RefSource, RefTrack};
use crate::core::keys::{Camelot, key_name};
use crate::core::query::{self, Dir, Expr, Sort};
use crate::core::selection::Selection;
use crate::core::suggest::{Direction, Scored};
use crate::core::track::TrackId;
use crate::ui::panels::CoreView;
use crate::ui::theme::THEME;
use crate::ui::widgets::{Seg, segmented};

/// Suggestions follow a tempo change once the tempo has held still this long.
const SETTLE: Duration = Duration::from_secs(1);

/// What the list was worked out for, apart from the tempo and key.
#[derive(Clone, Copy, PartialEq)]
struct Stamp {
    track: TrackId,
    deck: Option<usize>,
    playing: bool,
    direction: Direction,
    played: u64,
    lib: u64,
}

/// The tempo (to 0.1 BPM) and key the list was worked out for.
type Tempo = (i64, Option<Camelot>);

#[derive(Default)]
pub struct SuggestUi {
    pub source: RefSource,
    pub direction: Direction,
    /// Ranked, best first.
    pub list: Vec<Scored>,
    /// What the suggestions are measured from.
    pub reference: Option<RefTrack>,
    computed_for: Option<(Stamp, Tempo)>,
    /// A new tempo waiting to hold still before the list follows it, and since when.
    settling: Option<(Tempo, Instant)>,
    /// The rows shown: `list` filtered and sorted by the table.
    rows: Vec<TrackId>,
    sel: Selection,
    scroll_to: Option<usize>,
}

impl SuggestUi {
    pub fn focused(&self) -> Option<TrackId> {
        self.sel.focused()
    }

    pub fn move_focus(&mut self, delta: i32) {
        self.scroll_to = self.sel.move_focus(delta, &self.rows);
    }

    /// Work the list out again if the reference (as `source` picks it from the decks
    /// and Prepare's player, or `selected`) or the library changed. A tempo change
    /// alone waits until the tempo has held still for a moment, rather than
    /// re-ranking on every step of a fader sweep.
    pub fn refresh(
        &mut self,
        v: &CoreView,
        decks: &[DeckNow],
        prepare: Option<&DeckNow>,
        selected: Option<TrackId>,
    ) {
        let Some(r) = v
            .history
            .reference(v.lib, self.source, decks, prepare, selected)
        else {
            self.reference = None;
            self.list.clear();
            self.computed_for = None;
            return;
        };
        let stamp = Stamp {
            track: r.id,
            deck: r.deck,
            playing: r.playing,
            direction: self.direction,
            played: v.history.rev(),
            lib: v.lib.rev(),
        };
        let tempo: Tempo = ((r.bpm.unwrap_or(0.0) * 10.0).round() as i64, r.key);
        match self.computed_for {
            Some(c) if c == (stamp, tempo) => {
                self.settling = None;
                return;
            }
            Some((s, _)) if s == stamp => match self.settling {
                Some((t, since)) if t == tempo => {
                    if since.elapsed() < SETTLE {
                        return;
                    }
                }
                _ => {
                    self.settling = Some((tempo, Instant::now()));
                    return;
                }
            },
            _ => {}
        }
        self.settling = None;
        self.list = v
            .history
            .suggest(v.lib, &r, self.direction, &Default::default(), 80);
        self.reference = Some(r);
        self.computed_for = Some((stamp, tempo));
    }

    /// The list as the table shows it: filtered by its columns, sorted by its sort
    /// (or by score).
    fn rows(&self, v: &CoreView, table: &TableState) -> Vec<TrackId> {
        let filter = table.filter().unwrap_or(Expr::ALL);
        let cx = v.lib.context(v.collections);
        let mut rows: Vec<&Scored> = self
            .list
            .iter()
            .filter(|s| {
                v.lib
                    .get(s.id)
                    .is_some_and(|t| query::eval(&filter, t, &cx))
            })
            .collect();
        match table.sort {
            Some((ColumnId::Score, dir)) => {
                rows.sort_by(|a, b| a.total.total_cmp(&b.total));
                if dir == Dir::Desc {
                    rows.reverse();
                }
            }
            Some((ColumnId::Change, dir)) => {
                let ch = |s: &Scored| s.tempo_change.map_or(f64::MAX, f64::abs);
                rows.sort_by(|a, b| ch(a).total_cmp(&ch(b)));
                if dir == Dir::Desc {
                    rows.reverse();
                }
            }
            _ => {
                let sort: Sort = table.sort();
                if !sort.0.is_empty() {
                    rows.sort_by(|a, b| match (v.lib.get(a.id), v.lib.get(b.id)) {
                        (Some(x), Some(y)) => query::compare(&sort, x, y, &cx),
                        _ => std::cmp::Ordering::Equal,
                    });
                }
            }
        }
        rows.into_iter().map(|s| s.id).collect()
    }

    /// Returns true when the pane was clicked (so it takes keyboard focus).
    pub fn show(
        &mut self,
        ui: &mut Ui,
        v: &CoreView,
        has_focus: bool,
        media: &mut Media,
        table: &mut TableState,
        out: &mut Vec<Command>,
    ) -> bool {
        ui.horizontal(|ui| {
            let title = RichText::new("Suggestions").strong();
            ui.label(if has_focus {
                title.color(THEME.accent)
            } else {
                title
            })
            .on_hover_text("Tab switches the keyboard between the library and this list");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if let Some(d) = segmented(ui, self.direction, &directions()) {
                    self.direction = d;
                }
            });
        });
        // A deck that's no longer shown (back to two decks): follow Master instead.
        if matches!(self.source, RefSource::Deck(d) if d >= v.decks) {
            self.source = RefSource::Master;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("After").color(THEME.muted));
            let mut sources = vec![
                Seg::new(RefSource::Master, "Master")
                    .tip("Whichever deck is playing (in Prepare, the player while it plays)"),
            ];
            // One per deck shown: A and B, or A to D.
            for (d, (name, tip)) in DECK_SOURCES.iter().enumerate().take(v.decks) {
                sources.push(Seg::new(RefSource::Deck(d), name).tip(tip));
            }
            sources.push(
                Seg::new(RefSource::Selected, "Selected")
                    .tip("The track selected in the focused list, or the selected Flow card"),
            );
            if let Some(src) = segmented(ui, self.source, &sources) {
                self.source = src;
            }
        });
        ui.horizontal(|ui| match &self.reference {
            None => {
                let hint = match self.source {
                    RefSource::Master => "Nothing is playing".to_string(),
                    RefSource::Deck(d) => format!("Nothing on deck {}", letter(d)),
                    RefSource::Selected => "Select a track in the library".to_string(),
                };
                ui.label(RichText::new(hint).color(THEME.muted));
            }
            Some(r) => {
                match r.deck {
                    Some(d) => {
                        let (name, colour) = match DeckId::new(d) {
                            Some(_) => (format!("deck {}", letter(d)), THEME.deck[d.min(1)]),
                            None => ("Prepare's player".to_string(), THEME.accent),
                        };
                        let text = RichText::new(format!(
                            "{}{name}",
                            if r.playing { "LIVE " } else { "" }
                        ))
                        .color(colour);
                        ui.label(if r.playing { text.strong() } else { text })
                    }
                    None => ui.label(RichText::new("Selected").color(THEME.muted)),
                };
                let detail = format!(
                    "{} BPM  {}",
                    r.bpm.map_or("?".into(), |b| format!("{b:.1}")),
                    r.key.map_or("?".into(), key_name)
                );
                ui.label(RichText::new(detail).color(THEME.muted));
                if let Some(t) = v.lib.get(r.id) {
                    ui.add(egui::Label::new(&t.title).truncate());
                }
            }
        });
        ui.add_space(2.0);

        self.rows = self.rows(v, table);
        self.sel.retain(&self.rows);
        let scores: HashMap<TrackId, Scored> =
            self.list.iter().map(|s| (s.id, s.clone())).collect();
        let res = TrackTable {
            id: "suggestions",
            rows: &self.rows,
            state: table,
            available: TableView::Suggest.available(),
            sel: &mut self.sel,
            origin: Origin::Suggest,
            menu: MenuAt::default(),
            row_height: 34.0,
            reorder: false,
            takes_drops: false,
            scores: Some(&scores),
            scroll_to: self.scroll_to.take(),
            show_selection: has_focus,
        }
        .show(ui, v, media, out);
        res.clicked
    }
}

/// Drop, Hold and Build, for choosing which way the energy goes next.
pub fn directions() -> [Seg<'static, Direction>; 3] {
    [
        Seg::new(Direction::Drop, "Drop").tip("Less energy than now"),
        Seg::new(Direction::Hold, "Hold").tip("About the same energy"),
        Seg::new(Direction::Build, "Build").tip("More energy than now"),
    ]
}

/// A deck's letter by index.
/// Each deck's segment in "After": its letter, and its tooltip.
const DECK_SOURCES: [(&str, &str); 4] = [
    ("A", "The track on deck A, playing or not"),
    ("B", "The track on deck B, playing or not"),
    ("C", "The track on deck C, playing or not"),
    ("D", "The track on deck D, playing or not"),
];

fn letter(d: usize) -> String {
    DeckId::new(d)
        .and_then(deck_letter)
        .map_or_else(|| "?".into(), |l| l.to_string())
}
