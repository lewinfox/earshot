//! Pieces every track list shares: the one track menu, the title cell, the status
//! mark, and the waveform thumbnails and cover art (loaded once, used everywhere).

use std::collections::HashMap;

use earshot_engine::DeckId;
use egui::{Color32, RichText, Sense, TextureHandle, TextureId, Ui, vec2};

use crate::core::bus::deck_letter;
use crate::core::collections::{CollectionCmd, CollectionId, CollectionKind, FlowCmd, Section};
use crate::core::command::{Command, DeckCmd, PreviewCmd, ViewCmd};
use crate::core::edit::EditCmd;
use crate::core::index::LibraryCmd;
use crate::core::player::DECKS;
use crate::core::tempo_lock::TempoLockCmd;
use crate::core::track::{AnalysisState, Track, TrackId};
use crate::core::view::{Mode, Tool};
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
            Mark::Text("~".into(), THEME.bad),
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
/// (the selection, or just the row clicked outside it). Its parts, in order:
/// loading, adding to collections ([`add_to_menus`]), removing, editing the
/// metadata, then analysis. Tools (the tempo lock) go after analysis.
pub fn track_menu(ui: &mut Ui, sel: &[TrackId], v: &CoreView, at: MenuAt, out: &mut Vec<Command>) {
    let Some(&first) = sel.first() else { return };
    let n = sel.len();
    if n == 1 {
        load_items(ui, first, out);
        ui.separator();
    }
    add_to_menus(ui, sel, v, out);
    if let Some(c) = at.viewing.and_then(|id| v.collections.get(id))
        && c.kind.takes_tracks()
        && v.open_flow != Some(c.id)
        && ui
            .button(format!("{} from \"{}\"", count("Remove", n), c.name))
            .clicked()
    {
        out.push(
            CollectionCmd::RemoveTracks {
                from: c.id,
                tracks: sel.to_vec(),
            }
            .into(),
        );
        ui.close();
    }
    let edit = if n == 1 {
        "Edit…".to_string()
    } else {
        format!("Edit {n} tracks…")
    };
    if ui.button(edit).clicked() {
        out.push(EditCmd::Editor(sel.to_vec()).into());
        ui.close();
    }
    ui.separator();
    analyse_items(ui, sel, v, out);
    if let [first] = *sel
        && ui
            .button("Normalise tempo")
            .on_hover_text("Open it in Prepare's Tempo lock tab, to straighten its tempo")
            .clicked()
    {
        out.extend([
            ViewCmd::SetMode(Mode::Prepare).into(),
            ViewCmd::Tool(Tool::TempoLock).into(),
            TempoLockCmd::Analyse(first).into(),
        ]);
        ui.close();
    }
}

/// "Add" or "Add 3".
fn count(verb: &str, n: usize) -> String {
    if n == 1 {
        verb.to_string()
    } else {
        format!("{verb} {n}")
    }
}

/// Load onto a deck or Prepare's player, or preview in the headphones.
fn load_items(ui: &mut Ui, id: TrackId, out: &mut Vec<Command>) {
    let players = DeckId::first(DECKS).chain([DeckId::PREPARE]);
    for deck in players {
        let label = match deck_letter(deck) {
            Some(l) => format!("Load to deck {l}"),
            None => "Load to Prepare's player".to_string(),
        };
        if ui.button(label).clicked() {
            out.push(Command::Deck(deck, DeckCmd::Load(id)));
            ui.close();
        }
    }
    if ui.button("Preview in headphones").clicked() {
        out.push(Command::Preview(PreviewCmd::Toggle(id)));
        ui.close();
    }
}

/// "Add to Playlist ▸", "Add to Crate ▸", "Add to Flow ▸": each lists that tree, its
/// folders as submenus, with "New …" at the top. Added to a Flow, tracks are new
/// cards not linked to any other.
pub fn add_to_menus(ui: &mut Ui, sel: &[TrackId], v: &CoreView, out: &mut Vec<Command>) {
    let n = sel.len();
    for (section, kind) in [
        (Section::Playlists, CollectionKind::Playlist),
        (Section::Crates, CollectionKind::Crate),
        (Section::Flows, CollectionKind::Flow),
    ] {
        let title = format!("{} to {}", count("Add", n), title_case(kind.label()));
        ui.menu_button(title, |ui| {
            if ui.button(format!("New {}", kind.label())).clicked() {
                out.push(
                    CollectionCmd::Create {
                        kind,
                        parent: None,
                        tracks: sel.to_vec(),
                        query: None,
                    }
                    .into(),
                );
                ui.close();
            }
            if kind == CollectionKind::Flow && ui.button("The canvas").clicked() {
                // Whatever is on the canvas, opening it (a new Flow if it's empty).
                out.push(FlowCmd::AddTracks(sel.to_vec()).into());
                ui.close();
            }
            let any = v
                .collections
                .children_in(section, None)
                .iter()
                .any(|c| c.kind == kind || c.kind == CollectionKind::Folder);
            if any {
                ui.separator();
            }
            add_to_tree(ui, section, kind, None, sel, v, out);
        });
    }
}

/// One folder's level of an "Add to" menu: subfolders as submenus, then `kind`'s
/// collections.
fn add_to_tree(
    ui: &mut Ui,
    section: Section,
    kind: CollectionKind,
    folder: Option<CollectionId>,
    sel: &[TrackId],
    v: &CoreView,
    out: &mut Vec<Command>,
) {
    for c in v.collections.children_in(section, folder) {
        if c.kind == CollectionKind::Folder {
            ui.menu_button(&c.name, |ui| {
                add_to_tree(ui, section, kind, Some(c.id), sel, v, out);
                if !v
                    .collections
                    .children_in(section, Some(c.id))
                    .iter()
                    .any(|i| i.kind == kind || i.kind == CollectionKind::Folder)
                {
                    ui.label(RichText::new("(empty)").color(THEME.muted));
                }
            });
        } else if c.kind == kind {
            let label = if v.open_flow == Some(c.id) {
                format!("{}  (open)", c.name)
            } else {
                c.name.clone()
            };
            if ui.button(label).clicked() {
                // The app adds to the open Flow on its canvas, so it isn't overwritten.
                out.push(
                    CollectionCmd::AddTracks {
                        to: c.id,
                        tracks: sel.to_vec(),
                    }
                    .into(),
                );
                ui.close();
            }
        }
    }
}

/// "playlist" → "Playlist".
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// Analyse, again, dynamically; for several, the unanalysed ones.
fn analyse_items(ui: &mut Ui, sel: &[TrackId], v: &CoreView, out: &mut Vec<Command>) {
    let n = sel.len();
    let mut push = |ui: &mut Ui, tracks: Vec<TrackId>, dynamic| {
        out.push(LibraryCmd::Analyse { tracks, dynamic }.into());
        ui.close();
    };
    if let [first] = *sel {
        let t = v.lib.get(first);
        let label = if t.is_some_and(|t| t.analysed()) {
            "Analyse again"
        } else {
            "Analyse"
        };
        if ui.button(label).clicked() {
            push(ui, vec![first], false);
        }
        let hint = if t.is_some_and(|t| t.drifts) {
            "  (suggested)"
        } else {
            ""
        };
        if ui.button(format!("Analyse (dynamic){hint}")).clicked() {
            push(ui, vec![first], true);
        }
        return;
    }
    let fresh: Vec<TrackId> = sel
        .iter()
        .copied()
        .filter(|i| v.lib.get(*i).is_some_and(|t| !t.analysed()))
        .collect();
    if ui.button(format!("Analyse {n} tracks")).clicked() {
        push(ui, sel.to_vec(), false);
    }
    if ui
        .add_enabled(
            !fresh.is_empty(),
            egui::Button::new(format!("Analyse {} unanalysed", fresh.len())),
        )
        .clicked()
    {
        push(ui, fresh, false);
    }
    if ui.button(format!("Analyse {n} tracks (dynamic)")).clicked() {
        push(ui, sel.to_vec(), true);
    }
}
