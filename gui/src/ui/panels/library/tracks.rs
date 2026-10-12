//! Pieces every track list shares: the one track menu, the title cell, the status
//! mark, and the waveform thumbnails and cover art (loaded once, used everywhere).

use std::collections::HashMap;

use earshot_engine::DeckId;
use egui::{Color32, RichText, Sense, TextureHandle, TextureId, Ui, vec2};

use crate::core::bus::deck_letter;
use crate::core::collections::{CollectionCmd, CollectionId, CollectionKind, FlowCmd};
use crate::core::command::{Command, DeckCmd, PreviewCmd};
use crate::core::index::LibraryCmd;
use crate::core::player::DECKS;
use crate::core::track::{AnalysisState, Track, TrackId};
use crate::ui::panels::CoreView;
use crate::ui::panels::deck::wave;
use crate::ui::theme::THEME;

// ---- thumbnails and art ------------------------------------------------------------

/// Waveform thumbnails and cover art, by track. One cache for every list.
#[derive(Default)]
pub struct Media {
    previews: HashMap<TrackId, Vec<u8>>,
    thumbs: HashMap<TrackId, TextureHandle>,
    /// `None` while it loads, or when the file has none.
    pub art: HashMap<TrackId, Option<TextureHandle>>,
}

impl Media {
    /// New thumbnail data (from the Store): the texture is made again when next drawn.
    pub fn set_preview(&mut self, id: TrackId, bytes: Vec<u8>) {
        self.thumbs.remove(&id);
        self.previews.insert(id, bytes);
    }

    /// The track's waveform thumbnail, once it has one.
    pub fn thumb(&mut self, ctx: &egui::Context, id: TrackId) -> Option<TextureId> {
        let p = self.previews.get(&id)?;
        Some(
            self.thumbs
                .entry(id)
                .or_insert_with(|| wave::thumbnail(ctx, id, p))
                .id(),
        )
    }

    /// The cover, asking for it the first time.
    pub fn art(&self, ctx: &egui::Context, id: TrackId) -> Option<TextureId> {
        match self.art.get(&id) {
            Some(Some(tex)) => Some(tex.id()),
            Some(None) => None,
            None => {
                want_art(ctx, id);
                None
            }
        }
    }
}

/// Ask for a track's cover art; the app loads it in the background.
pub fn want_art(ctx: &egui::Context, id: TrackId) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<TrackId>>(art_wanted())
            .push(id)
    });
}

/// The tracks whose art was asked for since last time.
pub fn take_wanted_art(ctx: &egui::Context) -> Vec<TrackId> {
    ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<Vec<TrackId>>(art_wanted())))
}

fn art_wanted() -> egui::Id {
    egui::Id::new("earshot-art-wanted")
}

// ---- cells -------------------------------------------------------------------------

/// The title (dimmed once played this session), and the artist under it when
/// `two_lines`.
pub fn title_cell(ui: &mut Ui, t: &Track, played: bool, two_lines: bool) {
    let title = RichText::new(&t.title);
    let title = if played {
        title.color(THEME.muted)
    } else {
        title
    };
    if !two_lines {
        ui.add(egui::Label::new(title).truncate().selectable(false));
        return;
    }
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        ui.add(egui::Label::new(title).truncate().selectable(false));
        ui.add(
            egui::Label::new(RichText::new(&t.artist).size(11.0).color(THEME.muted))
                .truncate()
                .selectable(false),
        );
    });
}

/// The status column: painted shapes, since the built-in font lacks ● and friends.
pub fn status_mark(ui: &mut Ui, t: &Track, state: Option<&AnalysisState>) {
    enum Mark {
        Dot(Color32),
        Ring(Color32),
        Text(String, Color32),
        None,
    }
    let (mark, tip) = match state {
        Some(AnalysisState::Failed(e)) => (Mark::Text("!".into(), THEME.bad), e.clone()),
        Some(AnalysisState::Running { progress, stage }) => (
            Mark::Text(format!("{:.0}%", progress * 100.0), THEME.warn),
            stage.to_string(),
        ),
        Some(AnalysisState::Queued) => (Mark::Ring(THEME.warn), "Waiting to be analysed".into()),
        None if t.dynamic() => (
            Mark::Text("~".into(), THEME.ok),
            "Analysed (dynamic): the grid follows the tempo".into(),
        ),
        None if t.drifts => (
            Mark::Text("~".into(), THEME.warn),
            "The tempo seems to drift: right-click, Analyse (dynamic)".into(),
        ),
        None if t.analysed() => (
            Mark::Dot(THEME.ok),
            "Beatgrid analysed (static: one steady tempo)".into(),
        ),
        None => (Mark::None, "Not analysed".into()),
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    let c = rect.center();
    match mark {
        Mark::Dot(col) => {
            ui.painter().circle_filled(c, 3.5, col);
        }
        Mark::Ring(col) => {
            ui.painter()
                .circle_stroke(c, 3.5, egui::Stroke::new(1.2_f32, col));
        }
        Mark::Text(s, col) => {
            ui.painter().text(
                c,
                egui::Align2::CENTER_CENTER,
                s,
                egui::FontId::proportional(11.0),
                col,
            );
        }
        Mark::None => {}
    }
    resp.on_hover_text(tip);
}

// ---- the track menu ------------------------------------------------------------------

/// Where the menu was opened, for the actions that depend on it.
#[derive(Clone, Copy, Default)]
pub struct MenuAt {
    /// The crate, playlist or Flow being shown, for "Remove from".
    pub viewing: Option<CollectionId>,
}

/// The one right-click menu for tracks, in every list: `sel` is what it acts on
/// (the selection, or just the row clicked outside it).
pub fn track_menu(ui: &mut Ui, sel: &[TrackId], v: &CoreView, at: MenuAt, out: &mut Vec<Command>) {
    let Some(&first) = sel.first() else { return };
    let n = sel.len();
    let mut push = |ui: &mut Ui, c: Command| {
        out.push(c);
        ui.close();
    };
    if n == 1 {
        let players = DeckId::first(DECKS).chain([DeckId::PREPARE]);
        for deck in players {
            let label = match deck_letter(deck) {
                Some(l) => format!("Load to deck {l}"),
                None => "Load to Prepare's player".to_string(),
            };
            if ui.button(label).clicked() {
                push(ui, Command::Deck(deck, DeckCmd::Load(first)));
            }
        }
        if ui.button("Preview in headphones").clicked() {
            push(ui, Command::Preview(PreviewCmd::Toggle(first)));
        }
        ui.separator();
    }
    let count = |s: &str| {
        if n == 1 {
            s.to_string()
        } else {
            format!("{s} {n}")
        }
    };
    if ui.button(format!("{} to Flow", count("Add"))).clicked() {
        push(ui, FlowCmd::AddTracks(sel.to_vec()).into());
    }
    ui.menu_button(format!("{} to", count("Add")), |ui| {
        for kind in [CollectionKind::Crate, CollectionKind::Playlist] {
            if ui.button(format!("New {}", kind.label())).clicked() {
                let c = CollectionCmd::Create {
                    kind,
                    parent: None,
                    tracks: sel.to_vec(),
                    query: None,
                };
                push(ui, c.into());
            }
        }
        let takers: Vec<_> = v.collections.takers().collect();
        if !takers.is_empty() {
            ui.separator();
        }
        for c in takers {
            let label = format!("{}  ({})", c.name, c.kind.label());
            if ui.button(label).clicked() {
                let to = c.id;
                let cmd = match v.open_flow {
                    // The canvas has the open Flow; add there so it isn't overwritten.
                    Some(f) if f == to => FlowCmd::AddTracks(sel.to_vec()).into(),
                    _ => CollectionCmd::AddTracks {
                        to,
                        tracks: sel.to_vec(),
                    }
                    .into(),
                };
                push(ui, cmd);
            }
        }
    });
    if let Some(c) = at.viewing.and_then(|id| v.collections.get(id))
        && matches!(c.kind, CollectionKind::Crate | CollectionKind::Playlist)
        && ui
            .button(format!("{} from \"{}\"", count("Remove"), c.name))
            .clicked()
    {
        let from = c.id;
        push(
            ui,
            CollectionCmd::RemoveTracks {
                from,
                tracks: sel.to_vec(),
            }
            .into(),
        );
    }
    ui.separator();
    let analyse = |tracks: Vec<TrackId>, dynamic| LibraryCmd::Analyse { tracks, dynamic }.into();
    if n == 1 {
        let t = v.lib.get(first);
        let label = if t.is_some_and(|t| t.analysed()) {
            "Analyse again"
        } else {
            "Analyse"
        };
        if ui.button(label).clicked() {
            push(ui, analyse(vec![first], false));
        }
        let hint = if t.is_some_and(|t| t.drifts) {
            "  (suggested)"
        } else {
            ""
        };
        if ui.button(format!("Analyse (dynamic){hint}")).clicked() {
            push(ui, analyse(vec![first], true));
        }
        return;
    }
    let fresh: Vec<TrackId> = sel
        .iter()
        .copied()
        .filter(|i| v.lib.get(*i).is_some_and(|t| !t.analysed()))
        .collect();
    if ui.button(format!("Analyse {n} tracks")).clicked() {
        push(ui, analyse(sel.to_vec(), false));
    }
    if ui
        .add_enabled(
            !fresh.is_empty(),
            egui::Button::new(format!("Analyse {} unanalysed", fresh.len())),
        )
        .clicked()
    {
        push(ui, analyse(fresh, false));
    }
    if ui.button(format!("Analyse {n} tracks (dynamic)")).clicked() {
        push(ui, analyse(sel.to_vec(), true));
    }
}
