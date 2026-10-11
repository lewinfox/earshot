//! The library browser: a folder tree on the left (like Rekordbox's Explorer) and the
//! track list. Tracks can be multi-selected (Ctrl/Shift-click, Ctrl+A), dragged onto a
//! deck, double-clicked to load, and right-clicked to analyse.

use std::collections::{HashMap, HashSet};

use egui::{Align, Color32, RichText, Sense, TextureHandle, Ui, vec2};
use egui_extras::{Column, TableBuilder};

use super::deck::key_chip;
use super::util::{self, Camelot, Query};
use super::wave;
use crate::core::analysis::DYNAMIC_SOURCE;
use crate::core::db::LibTrack;
use crate::core::library::{self, Subdir};
use crate::core::suggest::Candidate;

const ANALYSED_SOURCES: [&str; 4] = ["analysis", DYNAMIC_SOURCE, "user", "tempo-lock sidecar"];

pub struct Track {
    pub lib: LibTrack,
    pub key: Option<Camelot>,
    /// Stand-in energy ranked across the library, 0..1.
    pub energy: f32,
    pub analysed: bool,
    pub dynamic: bool,
    pub drifts: bool,
    pub queued: bool,
    /// (progress, stage) while analysing.
    pub running: Option<(f64, String)>,
    pub error: Option<String>,
}

impl Track {
    fn new(lib: LibTrack) -> Track {
        let source = lib.grid_source.as_deref().unwrap_or("");
        Track {
            key: lib.key.as_deref().and_then(util::parse_key),
            energy: 0.5,
            analysed: ANALYSED_SOURCES.contains(&source),
            dynamic: source == DYNAMIC_SOURCE,
            drifts: lib.drifts,
            queued: false,
            running: None,
            error: None,
            lib,
        }
    }

    pub fn bpm(&self) -> Option<f64> {
        self.lib.bpm.or(self.lib.tag_bpm)
    }
}

pub enum LibAction {
    Load {
        deck: Option<usize>,
        track_id: i64,
    },
    Analyse {
        ids: Vec<i64>,
        dynamic: bool,
    },
    AddToFlow(Vec<i64>),
    /// Play a track in the headphones, or pause / resume it if it's the one loaded.
    Preview(i64),
    /// Preview a track from this far in (0..1), starting it if needed.
    PreviewSeek(i64, f32),
    /// Stop the preview if it is this track.
    PreviewStop(i64),
    RemoveFolder(String),
}

/// Ask for a track's cover art; the app loads it in the background.
pub fn want_art(ctx: &egui::Context, id: i64) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<i64>>(art_wanted()).push(id));
}

/// The tracks whose art was asked for since last time.
pub fn take_wanted_art(ctx: &egui::Context) -> Vec<i64> {
    ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<Vec<i64>>(art_wanted())))
}

fn art_wanted() -> egui::Id {
    egui::Id::new("earshot-art-wanted")
}

/// The track in the headphones' preview player.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PreviewState {
    pub id: i64,
    /// How far through, 0..1.
    pub at: f32,
    /// Playing (or about to, while it decodes) rather than paused or finished.
    pub playing: bool,
}

#[derive(Default)]
pub struct LibraryUi {
    pub tracks: Vec<Track>,
    pub by_id: HashMap<i64, usize>,
    pub folders: Vec<String>,
    pub folder: Option<String>,
    subdirs: HashMap<String, Vec<Subdir>>,
    pub search: String,
    /// Indices into `tracks` of the rows shown.
    pub shown: Vec<usize>,
    dirty: bool,
    chosen: HashSet<i64>,
    /// Row (index into `shown`) with keyboard focus.
    focus: Option<usize>,
    anchor: Option<usize>,
    scroll_to: Option<usize>,
    pub previews: HashMap<i64, Vec<u8>>,
    /// Cover art by track: `None` while it loads, or when the file has none.
    pub art: HashMap<i64, Option<TextureHandle>>,
    thumbs: HashMap<i64, TextureHandle>,
    pub scanning: bool,
    pub search_focus: bool,
    /// Tracks played this session (shown dimmed).
    pub played: HashSet<i64>,
    /// Bumped whenever track data changes (rescan, analysis), so derived lists refresh.
    pub version: u64,
}

impl LibraryUi {
    pub fn set_tracks(&mut self, tracks: Vec<LibTrack>) {
        // Keep analysis progress for tracks still being worked on.
        let old: HashMap<i64, Track> = self.tracks.drain(..).map(|t| (t.lib.id, t)).collect();
        self.tracks = tracks
            .into_iter()
            .map(|l| {
                let mut t = Track::new(l);
                if let Some(o) = old.get(&t.lib.id) {
                    (t.queued, t.running, t.error) = (o.queued, o.running.clone(), o.error.clone());
                }
                t
            })
            .collect();
        self.by_id = self
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.lib.id, i))
            .collect();
        // Rank stand-in energy across the library.
        let mut known: Vec<(f64, usize)> = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, t)| t.lib.energy.map(|e| (e, i)))
            .collect();
        known.sort_by(|a, b| a.0.total_cmp(&b.0));
        let n = known.len();
        for (rank, (_, i)) in known.into_iter().enumerate() {
            self.tracks[i].energy = if n > 1 {
                rank as f32 / (n - 1) as f32
            } else {
                0.5
            };
        }
        self.dirty = true;
        self.version += 1;
    }

    /// Every track, as the suggestion scorer sees it.
    pub fn candidates(&self) -> Vec<Candidate> {
        self.tracks
            .iter()
            .map(|t| Candidate {
                id: t.lib.id,
                bpm: t.bpm(),
                key: t.key,
                energy: t.energy,
            })
            .collect()
    }

    pub fn track_mut(&mut self, id: i64) -> Option<&mut Track> {
        self.by_id.get(&id).map(|&i| &mut self.tracks[i])
    }

    pub fn track(&self, id: i64) -> Option<&Track> {
        self.by_id.get(&id).map(|&i| &self.tracks[i])
    }

    pub fn invalidate_thumb(&mut self, id: i64) {
        self.thumbs.remove(&id);
    }

    fn refilter(&mut self) {
        let q = Query::parse(&self.search);
        let dir = self.folder.as_ref().map(|f| format!("{f}/"));
        let focused = self.focus.and_then(|f| self.shown.get(f)).copied();
        self.shown = (0..self.tracks.len())
            .filter(|&i| {
                let t = &self.tracks[i];
                dir.as_ref()
                    .is_none_or(|d| t.lib.path.starts_with(d.as_str()))
                    && q.matches(&t.lib.title, &t.lib.artist, t.bpm(), t.key)
            })
            .collect();
        self.focus = focused.and_then(|f| self.shown.iter().position(|&i| i == f));
        let ids: HashSet<i64> = self.shown.iter().map(|&i| self.tracks[i].lib.id).collect();
        self.chosen.retain(|id| ids.contains(id));
        self.dirty = false;
    }

    /// The track with keyboard focus (for "load selected").
    pub fn focused(&self) -> Option<i64> {
        self.focus
            .and_then(|f| self.shown.get(f))
            .map(|&i| self.tracks[i].lib.id)
    }

    pub fn move_focus(&mut self, delta: i32) {
        if self.shown.is_empty() {
            return;
        }
        let f = self.focus.map_or(0, |f| {
            (f as i32 + delta).clamp(0, self.shown.len() as i32 - 1) as usize
        });
        self.focus = Some(f);
        self.anchor = Some(f);
        self.chosen = HashSet::from([self.tracks[self.shown[f]].lib.id]);
        self.scroll_to = Some(f);
    }

    // ---- folder tree ----------------------------------------------------------------------

    /// `flows` draws the saved Flows between *All tracks* and the folders.
    pub fn tree(&mut self, ui: &mut Ui, actions: &mut Vec<LibAction>, flows: impl FnOnce(&mut Ui)) {
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                if ui
                    .selectable_label(self.folder.is_none(), "All tracks")
                    .clicked()
                {
                    self.folder = None;
                    self.dirty = true;
                }
                ui.add_space(6.0);
                flows(ui);
                ui.add_space(6.0);
                ui.label(RichText::new("EXPLORER").size(10.0).color(util::MUTED));
                if self.folders.is_empty() {
                    ui.label(
                        RichText::new("No folders yet. Menu > Import > Folder...")
                            .size(11.0)
                            .color(util::MUTED),
                    );
                }
                for root in self.folders.clone() {
                    let name = root.rsplit('/').next().unwrap_or(&root).to_string();
                    self.node(
                        ui,
                        &Subdir {
                            name,
                            path: root,
                            has_children: true,
                        },
                        true,
                        actions,
                    );
                }
            });
    }

    fn label(&mut self, ui: &mut Ui, d: &Subdir, root: bool, actions: &mut Vec<LibAction>) {
        let selected = self.folder.as_deref() == Some(d.path.as_str());
        let text = if root {
            RichText::new(&d.name).strong()
        } else {
            RichText::new(&d.name)
        };
        let r = ui.selectable_label(selected, text).on_hover_text(&d.path);
        if r.clicked() {
            self.folder = Some(d.path.clone());
            self.dirty = true;
        }
        r.context_menu(|ui| self.folder_menu(ui, &d.path, root, actions));
    }

    fn node(&mut self, ui: &mut Ui, d: &Subdir, root: bool, actions: &mut Vec<LibAction>) {
        if !d.has_children {
            ui.horizontal(|ui| {
                ui.add_space(ui.spacing().indent);
                self.label(ui, d, root, actions);
            });
            return;
        }
        let id = ui.make_persistent_id(("folder", &d.path));
        let state =
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
        let header = state.show_header(ui, |ui| self.label(ui, d, root, actions));
        header.body(|ui| {
            let kids = self
                .subdirs
                .entry(d.path.clone())
                .or_insert_with(|| library::list_subdirs(&d.path).unwrap_or_default())
                .clone();
            for k in &kids {
                self.node(ui, k, false, actions);
            }
        });
    }

    fn folder_menu(&mut self, ui: &mut Ui, path: &str, root: bool, actions: &mut Vec<LibAction>) {
        let prefix = format!("{path}/");
        let inside: Vec<&Track> = self
            .tracks
            .iter()
            .filter(|t| t.lib.path.starts_with(&prefix))
            .collect();
        let fresh: Vec<i64> = inside
            .iter()
            .filter(|t| !t.analysed)
            .map(|t| t.lib.id)
            .collect();
        let drifting: Vec<i64> = inside
            .iter()
            .filter(|t| t.drifts)
            .map(|t| t.lib.id)
            .collect();
        let all: Vec<i64> = inside.iter().map(|t| t.lib.id).collect();
        if ui
            .add_enabled(
                !fresh.is_empty(),
                egui::Button::new(format!("Analyse {} unanalysed tracks", fresh.len())),
            )
            .clicked()
        {
            actions.push(LibAction::Analyse {
                ids: fresh,
                dynamic: false,
            });
            ui.close();
        }
        if ui
            .button(format!("Analyse all {} tracks again", all.len()))
            .clicked()
        {
            actions.push(LibAction::Analyse {
                ids: all,
                dynamic: false,
            });
            ui.close();
        }
        if ui
            .add_enabled(
                !drifting.is_empty(),
                egui::Button::new(format!(
                    "Analyse {} drifting tracks (dynamic)",
                    drifting.len()
                )),
            )
            .clicked()
        {
            actions.push(LibAction::Analyse {
                ids: drifting,
                dynamic: true,
            });
            ui.close();
        }
        if root {
            ui.separator();
            if ui.button("Remove from library").clicked() {
                actions.push(LibAction::RemoveFolder(path.to_string()));
                ui.close();
            }
        }
    }

    // ---- track list -----------------------------------------------------------------------

    /// Returns true when a row was clicked (so the list takes keyboard focus).
    pub fn list(&mut self, ui: &mut Ui, actions: &mut Vec<LibAction>) -> bool {
        if self.dirty {
            self.refilter();
        }
        ui.horizontal(|ui| {
            let title = self
                .folder
                .as_deref()
                .map_or("All tracks", |f| f.rsplit('/').next().unwrap_or(f));
            ui.label(RichText::new(title).strong());
            let unanalysed: Vec<i64> = self
                .shown
                .iter()
                .map(|&i| &self.tracks[i])
                .filter(|t| !t.analysed)
                .map(|t| t.lib.id)
                .collect();
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let label = if unanalysed.is_empty() {
                    "All analysed".to_string()
                } else {
                    format!("Analyse all ({})", unanalysed.len())
                };
                if ui
                    .add_enabled(!unanalysed.is_empty(), egui::Button::new(label))
                    .on_hover_text("Analyse every unanalysed track in this list (static mode)")
                    .clicked()
                {
                    actions.push(LibAction::Analyse {
                        ids: unanalysed,
                        dynamic: false,
                    });
                }
                let count = if self.scanning {
                    "scanning…".into()
                } else {
                    format!("{} tracks", self.shown.len())
                };
                ui.label(RichText::new(count).color(util::MUTED));
                let search = ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .hint_text("Search   bpm:120-126  key:8A   ( / )")
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut self.search_focus) {
                    search.request_focus();
                }
                if search.changed() {
                    self.dirty = true;
                }
            });
        });
        if self.dirty {
            self.refilter();
        }
        if self.folders.is_empty() && !self.scanning {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No library folders yet. Menu > Import > Folder...")
                        .color(util::MUTED),
                )
            });
            return false;
        }

        let ctrl = ui.input(|i| i.modifiers.command);
        if ctrl && ui.input(|i| i.key_pressed(egui::Key::A)) && !ui.ctx().wants_keyboard_input() {
            self.chosen = self.shown.iter().map(|&i| self.tracks[i].lib.id).collect();
        }
        let mut table = TableBuilder::new(ui)
            .striped(true)
            .sense(Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(Align::Center))
            .column(Column::exact(34.0))
            .column(Column::exact(100.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(
                Column::initial(200.0)
                    .at_least(80.0)
                    .resizable(true)
                    .clip(true),
            )
            .column(Column::exact(56.0))
            .column(Column::exact(40.0))
            .column(Column::exact(44.0));
        if let Some(r) = self.scroll_to.take() {
            table = table.scroll_to_row(r, Some(Align::Center));
        }
        let shown = self.shown.clone();
        let mut clicked: Option<(usize, egui::Modifiers)> = None;
        let mut right_clicked: Option<usize> = None;
        table
            .header(20.0, |mut h| {
                for name in ["", "", "Title", "Artist", "BPM", "Key", "Time"] {
                    h.col(|ui| {
                        ui.label(RichText::new(name).size(11.0).color(util::MUTED));
                    });
                }
            })
            .body(|body| {
                body.rows(26.0, shown.len(), |mut row| {
                    let r = row.index();
                    let t = &self.tracks[shown[r]];
                    let id = t.lib.id;
                    row.set_selected(self.chosen.contains(&id));
                    row.col(|ui| status(ui, t));
                    row.col(|ui| {
                        if let Some(p) = self.previews.get(&id) {
                            let tex = self
                                .thumbs
                                .entry(id)
                                .or_insert_with(|| wave::thumbnail(ui.ctx(), id, p));
                            ui.add(egui::Image::new((tex.id(), vec2(96.0, 18.0))));
                        }
                    });
                    row.col(|ui| {
                        let title = RichText::new(&t.lib.title);
                        let title = if self.played.contains(&id) {
                            title.color(util::MUTED)
                        } else {
                            title
                        };
                        ui.add(egui::Label::new(title).truncate().selectable(false));
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&t.lib.artist).color(util::MUTED))
                                .truncate()
                                .selectable(false),
                        );
                    });
                    row.col(|ui| {
                        let bpm = t
                            .bpm()
                            .map(|b| format!("{b:.2}"))
                            .unwrap_or_else(|| "-".into());
                        let text = RichText::new(bpm).color(util::MUTED);
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(if t.analysed { text } else { text.italics() })
                                    .selectable(false),
                            )
                            .on_hover_text(if t.analysed {
                                "Analysed"
                            } else {
                                "From the file's tag (not analysed)"
                            });
                        });
                    });
                    row.col(|ui| {
                        if let Some(k) = t.key {
                            key_chip(ui, k);
                        }
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(t.lib.duration.map(util::mmss).unwrap_or_default())
                                .color(util::MUTED),
                        );
                    });
                    let resp = row.response();
                    if resp.clicked() {
                        clicked = Some((r, ui_mods(&resp)));
                    }
                    if resp.double_clicked() {
                        actions.push(LibAction::Load {
                            deck: None,
                            track_id: id,
                        });
                    }
                    if resp.secondary_clicked() {
                        right_clicked = Some(r);
                    }
                    resp.dnd_set_drag_payload(id);
                    resp.context_menu(|ui| self.track_menu(ui, id, actions));
                });
            });
        if let Some(r) = right_clicked {
            let id = self.tracks[shown[r]].lib.id;
            // Right-click inside the selection acts on all of it; outside, just that row.
            if !self.chosen.contains(&id) {
                self.select(r);
            }
        }
        let any_click = clicked.is_some() || right_clicked.is_some();
        if let Some((r, m)) = clicked {
            let id = self.tracks[shown[r]].lib.id;
            if m.shift
                && let Some(a) = self.anchor
            {
                let (lo, hi) = (a.min(r), a.max(r));
                self.chosen = shown[lo..=hi]
                    .iter()
                    .map(|&i| self.tracks[i].lib.id)
                    .collect();
                self.focus = Some(r);
            } else if m.command {
                if !self.chosen.remove(&id) {
                    self.chosen.insert(id);
                }
                self.focus = Some(r);
                self.anchor = Some(r);
            } else {
                self.select(r);
            }
        }
        any_click
    }

    fn select(&mut self, r: usize) {
        self.focus = Some(r);
        self.anchor = Some(r);
        self.chosen = HashSet::from([self.tracks[self.shown[r]].lib.id]);
    }

    fn track_menu(&self, ui: &mut Ui, id: i64, actions: &mut Vec<LibAction>) {
        let chosen: Vec<i64> = self
            .shown
            .iter()
            .map(|&i| self.tracks[i].lib.id)
            .filter(|i| self.chosen.contains(i))
            .collect();
        if chosen.len() > 1 {
            if ui.button(format!("Add {} to Flow", chosen.len())).clicked() {
                actions.push(LibAction::AddToFlow(chosen.clone()));
                ui.close();
            }
            ui.separator();
            let fresh: Vec<i64> = chosen
                .iter()
                .copied()
                .filter(|i| self.track(*i).is_some_and(|t| !t.analysed))
                .collect();
            if ui
                .button(format!("Analyse {} tracks", chosen.len()))
                .clicked()
            {
                actions.push(LibAction::Analyse {
                    ids: chosen.clone(),
                    dynamic: false,
                });
                ui.close();
            }
            if ui
                .add_enabled(
                    !fresh.is_empty(),
                    egui::Button::new(format!("Analyse {} unanalysed", fresh.len())),
                )
                .clicked()
            {
                actions.push(LibAction::Analyse {
                    ids: fresh,
                    dynamic: false,
                });
                ui.close();
            }
            if ui
                .button(format!("Analyse {} tracks (dynamic)", chosen.len()))
                .clicked()
            {
                actions.push(LibAction::Analyse {
                    ids: chosen,
                    dynamic: true,
                });
                ui.close();
            }
            return;
        }
        let Some(t) = self.track(id) else { return };
        for (deck, label) in [(0, "Load to deck A"), (1, "Load to deck B")] {
            if ui.button(label).clicked() {
                actions.push(LibAction::Load {
                    deck: Some(deck),
                    track_id: id,
                });
                ui.close();
            }
        }
        if ui.button("Preview in headphones").clicked() {
            actions.push(LibAction::Preview(id));
            ui.close();
        }
        if ui.button("Add to Flow").clicked() {
            actions.push(LibAction::AddToFlow(vec![id]));
            ui.close();
        }
        ui.separator();
        if ui
            .button(if t.analysed {
                "Analyse again"
            } else {
                "Analyse"
            })
            .clicked()
        {
            actions.push(LibAction::Analyse {
                ids: vec![id],
                dynamic: false,
            });
            ui.close();
        }
        if ui
            .button(format!(
                "Analyse (dynamic){}",
                if t.drifts { "  (suggested)" } else { "" }
            ))
            .clicked()
        {
            actions.push(LibAction::Analyse {
                ids: vec![id],
                dynamic: true,
            });
            ui.close();
        }
    }
}

fn ui_mods(r: &egui::Response) -> egui::Modifiers {
    r.ctx.input(|i| i.modifiers)
}

/// The status column: painted shapes, since the built-in font lacks ● and friends.
fn status(ui: &mut Ui, t: &Track) {
    enum Mark {
        Dot(Color32),
        Ring(Color32),
        Text(String, Color32),
        None,
    }
    let (mark, tip) = if let Some(e) = &t.error {
        (Mark::Text("!".into(), util::BAD), e.clone())
    } else if let Some((p, stage)) = &t.running {
        (
            Mark::Text(format!("{:.0}%", p * 100.0), util::WARN),
            stage.clone(),
        )
    } else if t.queued {
        (Mark::Ring(util::WARN), "Waiting to be analysed".into())
    } else if t.dynamic {
        (
            Mark::Text("~".into(), util::OK),
            "Analysed (dynamic): the grid follows the tempo".into(),
        )
    } else if t.drifts {
        (
            Mark::Text("~".into(), util::WARN),
            "The tempo seems to drift: right-click, Analyse (dynamic)".into(),
        )
    } else if t.analysed {
        (
            Mark::Dot(util::OK),
            "Beatgrid analysed (static: one steady tempo)".into(),
        )
    } else {
        (Mark::None, "Not analysed".into())
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 18.0), Sense::hover());
    let c = rect.center();
    match mark {
        Mark::Dot(col) => {
            ui.painter().circle_filled(c, 3.5, col);
        }
        Mark::Ring(col) => {
            ui.painter()
                .circle_stroke(c, 3.5, egui::Stroke::new(1.2, col));
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
