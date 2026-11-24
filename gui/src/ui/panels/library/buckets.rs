//! Bucket sources in the Sources pane (`core::bucket`): the S3-compatible buckets
//! added, each under the name the user gave it, and the window that adds one (or
//! changes one's name, service or key).

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use egui::{RichText, TextEdit, Ui};

use super::browser::Scope;
use crate::core::bucket::{BucketCmd, BucketForm, Scan};
use crate::core::command::Command;
use crate::core::index::LibraryCmd;
use crate::core::sources::{Source, SourceId};
use crate::core::view::Source as Pane;
use crate::ui::modals::show_fitted;
use crate::ui::panels::CoreView;
use crate::ui::theme::THEME;
use crate::ui::widgets::StackItem;
use crate::ui::widgets::{Button, Note, busy_line, muted};

/// The window's title: adding a bucket, or changing one.
const ADD: &str = "Add a bucket";
const EDIT: &str = "Edit bucket";

/// The window that adds or changes a bucket.
struct Dialog {
    /// The source being changed; `None`: a new one.
    source: Option<SourceId>,
    form: BucketForm,
    /// Sent, and waiting to hear whether the bucket could be listed.
    checking: bool,
    /// Why it couldn't be saved, last time.
    error: Option<String>,
}

#[derive(Default)]
pub struct BucketsUi {
    /// The bucket sources, in the Store's order.
    sources: Vec<Source>,
    dialog: Option<Dialog>,
    /// Being read through now.
    scanning: HashSet<SourceId>,
    /// How each one's last read went: what it found, or why it failed.
    last: HashMap<SourceId, Result<Scan, String>>,
    /// The computer is offline: only downloaded tracks are listed.
    offline: bool,
    /// The folders of the bucket whose pane is open, as of a library revision
    /// ([`BucketsUi::folders`]).
    tree: Option<(SourceId, u64, std::rc::Rc<Vec<String>>)>,
}

impl BucketsUi {
    /// Every source, whenever they change: the buckets among them are listed.
    pub fn set_sources(&mut self, sources: &[Source]) {
        self.sources = sources
            .iter()
            .filter(|s| s.kind.is_bucket() && s.enabled)
            .cloned()
            .collect();
        self.last
            .retain(|id, _| self.sources.iter().any(|s| s.id == *id));
    }

    /// Whether there are any (the collections' menus then offer to keep them offline).
    pub fn any(&self) -> bool {
        !self.sources.is_empty()
    }

    /// The computer went offline, or is online again.
    pub fn set_online(&mut self, online: bool) {
        self.offline = !online;
    }

    /// A bucket is being read through.
    pub fn scanning(&mut self, source: SourceId) {
        self.scanning.insert(source);
    }

    /// It has been, or couldn't be.
    pub fn scanned(&mut self, source: SourceId, result: &Result<Scan, String>) {
        self.scanning.remove(&source);
        self.last.insert(source, result.clone());
    }

    /// The bucket in the window was saved (the window closes), or couldn't be (it
    /// says why).
    pub fn saved(&mut self, result: &Result<SourceId, String>) {
        match (result, &mut self.dialog) {
            (Ok(_), _) => self.dialog = None,
            (Err(e), Some(d)) => {
                d.checking = false;
                d.error = Some(e.clone());
            }
            (Err(_), None) => {}
        }
    }

    /// Open the window to add a bucket.
    pub fn add(&mut self) {
        self.dialog = Some(Dialog {
            source: None,
            form: BucketForm::default(),
            checking: false,
            error: None,
        });
    }

    #[cfg(test)]
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    /// The bucket sources' icons for the left icon stack: one each, by the first
    /// letters of the name the user gave it (as the DJ programs have "RB", "MX").
    pub fn stack_items(&self) -> Vec<StackItem<Pane>> {
        self.sources
            .iter()
            .enumerate()
            .map(|(i, s)| StackItem {
                id: Pane::Bucket(s.id),
                icon: Cow::Owned(initials(&s.name)),
                tip: Cow::Owned(format!("{}: a bucket", s.name)),
                enabled: true,
                // Reading it through now.
                light: self.scanning.contains(&s.id).then_some(THEME.warn),
                gap: i == 0,
                menu: true,
            })
            .collect()
    }

    /// The bucket a pane shows is gone (removed).
    pub fn pane_gone(&self, id: SourceId) -> bool {
        !self.sources.iter().any(|s| s.id == id)
    }

    /// What can be done to a bucket: its icon's right-click menu.
    pub fn menu(&mut self, ui: &mut Ui, id: SourceId, out: &mut Vec<Command>) {
        let Some(s) = self.sources.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        if ui.button("Read again").clicked() {
            out.push(BucketCmd::Scan(s.id).into());
            ui.close();
        }
        if ui.button("Edit…").clicked() {
            self.edit(&s);
            ui.close();
        }
        ui.separator();
        if ui.button("Remove").on_hover_text(REMOVE_TIP).clicked() {
            out.push(LibraryCmd::RemoveSource(s.id).into());
            ui.close();
        }
    }

    /// Open the window to change a bucket's name, service or key.
    fn edit(&mut self, s: &Source) {
        self.dialog = Some(Dialog {
            source: Some(s.id),
            form: BucketForm::of(s),
            checking: false,
            error: None,
        });
    }

    /// The Manage pane's Buckets section: just the button that adds one. Each bucket
    /// added has an icon of its own in the stack.
    pub fn manage(&mut self, ui: &mut Ui) {
        if ui
            .button("Add a bucket…")
            .on_hover_text("Amazon S3, Cloudflare R2, Backblaze B2, Wasabi, MinIO")
            .clicked()
        {
            self.add();
        }
        muted(ui, "Each bucket gets an icon of its own on the left.");
    }

    /// A bucket's own pane: what it is, how much of it is downloaded, what can be
    /// done to it, and its folders (picking one narrows the list to it). Returns
    /// the scope picked, if one was.
    pub fn pane(
        &mut self,
        ui: &mut Ui,
        id: SourceId,
        v: &CoreView,
        scope: &Scope,
        out: &mut Vec<Command>,
    ) -> Option<Scope> {
        let s = self.sources.iter().find(|s| s.id == id).cloned()?;
        let root = s.bucket_url()?;
        let mut picked = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.label(RichText::new(&s.name).strong().size(14.0));
                let endpoint = s
                    .config
                    .s3
                    .as_ref()
                    .and_then(|c| c.endpoint.clone())
                    .unwrap_or_else(|| "Amazon S3".into());
                muted(ui, &root).on_hover_text(endpoint);
                // Its tracks, and how many of them are downloaded.
                let inside = format!("{root}/");
                let (mut all, mut here) = (0, 0);
                for t in v
                    .lib
                    .tracks()
                    .iter()
                    .filter(|t| t.path.starts_with(&inside))
                {
                    all += 1;
                    here += usize::from(v.lib.downloaded(t.id) == Some(true));
                }
                muted(ui, format!("{}, {here} downloaded", tracks(all)));
                ui.add_space(4.0);
                if self.offline {
                    ui.add(
                        Note::warning(
                            "This computer is offline: only the tracks already downloaded \
                             are listed. The rest are back when it's online.",
                        )
                        .small(),
                    );
                }
                if self.scanning.contains(&s.id) {
                    busy_line(ui, "", "Reading…");
                } else {
                    match self.last.get(&s.id) {
                        Some(Err(e)) => {
                            ui.add(Note::danger(e).small());
                        }
                        Some(Ok(scan)) if scan.failed > 0 => {
                            ui.add(
                                Note::warning(format!(
                                    "{} couldn't be read; Read again to try them",
                                    tracks(scan.failed)
                                ))
                                .small(),
                            );
                        }
                        _ => {}
                    }
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button("Read again")
                        .on_hover_text("List the bucket, and read what's new or changed")
                        .clicked()
                    {
                        out.push(BucketCmd::Scan(s.id).into());
                    }
                    if ui.button("Edit…").clicked() {
                        self.edit(&s);
                    }
                    if ui.button("Remove").on_hover_text(REMOVE_TIP).clicked() {
                        out.push(LibraryCmd::RemoveSource(s.id).into());
                    }
                });
                ui.add_space(8.0);
                // Everything in it, then its folders.
                let whole = matches!(scope, Scope::Folder(f) if *f == root);
                if ui
                    .selectable_label(whole, format!("All of it  ({all})"))
                    .clicked()
                {
                    picked = Some(Scope::Folder(root.clone()));
                }
                let folders = self.folders(&s, &root, v);
                self.folder_rows(ui, &root, "", &folders, scope, &mut picked);
            });
        picked
    }

    /// The folders in a bucket (each key's, and their parents'), relative to its
    /// root and sorted: worked out again only when the library changes.
    fn folders(&mut self, s: &Source, root: &str, v: &CoreView) -> std::rc::Rc<Vec<String>> {
        let rev = v.lib.rev();
        if let Some((id, at, folders)) = &self.tree
            && (*id, *at) == (s.id, rev)
        {
            return folders.clone();
        }
        let inside = format!("{root}/");
        let mut set = std::collections::BTreeSet::new();
        for t in v.lib.tracks() {
            let Some(key) = t.path.strip_prefix(&inside) else {
                continue;
            };
            // Every folder above the file.
            let mut dir = key;
            while let Some((parent, _)) = dir.rsplit_once('/') {
                if !set.insert(parent.to_string()) {
                    break;
                }
                dir = parent;
            }
        }
        let folders = std::rc::Rc::new(set.into_iter().collect::<Vec<_>>());
        self.tree = Some((s.id, rev, folders.clone()));
        folders
    }

    /// The folders directly in `parent` ("" for the bucket's root), each with its own
    /// inside it.
    fn folder_rows(
        &self,
        ui: &mut Ui,
        root: &str,
        parent: &str,
        folders: &[String],
        scope: &Scope,
        picked: &mut Option<Scope>,
    ) {
        let children = folders.iter().filter(|f| match f.rsplit_once('/') {
            Some((p, _)) => p == parent,
            None => parent.is_empty(),
        });
        for f in children {
            let name = f.rsplit('/').next().unwrap_or(f);
            let url = format!("{root}/{f}");
            let selected = matches!(scope, Scope::Folder(s) if *s == url);
            let prefix = format!("{f}/");
            let has_children = folders.iter().any(|o| o.starts_with(&prefix));
            let mut label = |ui: &mut Ui| {
                if ui
                    .selectable_label(selected, name)
                    .on_hover_text(&url)
                    .clicked()
                {
                    *picked = Some(Scope::Folder(url.clone()));
                }
            };
            if !has_children {
                ui.horizontal(|ui| {
                    ui.add_space(ui.spacing().indent);
                    label(ui);
                });
                continue;
            }
            let id = ui.make_persistent_id(("bucket-folder", &url));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, label)
                .body(|ui| self.folder_rows(ui, root, f, folders, scope, picked));
        }
    }

    /// The window that adds or changes a bucket, while it's open.
    pub fn window(&mut self, ctx: &egui::Context, out: &mut Vec<Command>) {
        let Some(d) = &mut self.dialog else { return };
        let new = d.source.is_none();
        let mut open = true;
        let mut close = false;
        show_fitted(
            ctx,
            egui::Window::new(if new { ADD } else { EDIT })
                // In the middle of the app, clear of the pane it was opened from.
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.content_rect().center())
                .open(&mut open)
                .resizable(false)
                .collapsible(false),
            |ui| {
                ui.set_width(420.0);
                ui.add_enabled_ui(!d.checking, |ui| fields(ui, &mut d.form, new));
                ui.add_space(6.0);
                muted(
                    ui,
                    "A read-only key is enough: earshot only lists and downloads. The key \
                     is kept in the system keychain, not in earshot's library.",
                );
                if let Some(e) = &d.error {
                    ui.add_space(4.0);
                    ui.add(Note::danger(e));
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let problem = d.form.problem(new);
                    let go = ui
                        .add_enabled(
                            !d.checking && problem.is_none(),
                            Button::new(if new { "Add" } else { "Save" }),
                        )
                        .on_disabled_hover_text(problem.unwrap_or("Checking…"));
                    if go.clicked() {
                        d.checking = true;
                        d.error = None;
                        out.push(
                            BucketCmd::Save {
                                form: d.form.clone(),
                                source: d.source,
                            }
                            .into(),
                        );
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if d.checking {
                        busy_line(ui, "", "Checking the bucket can be listed…");
                    }
                });
            },
        );
        if !open || close {
            self.dialog = None;
        }
    }
}

/// What removing a bucket does, for its buttons' hover text.
const REMOVE_TIP: &str = "Its tracks leave the library, its downloads are deleted and its \
                          access key is forgotten. Nothing in the bucket is touched.";

/// A bucket's icon: the first letters of the first two words of its name ("Gig box":
/// "GB"), or of its one word ("Archive": "AR").
fn initials(name: &str) -> String {
    let mut words = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty());
    let letters: String = match (words.next(), words.next()) {
        (Some(a), Some(b)) => a.chars().take(1).chain(b.chars().take(1)).collect(),
        (Some(a), None) => a.chars().take(2).collect(),
        _ => "S3".into(),
    };
    letters.to_uppercase()
}

/// "1 track", "3 tracks".
fn tracks(n: usize) -> String {
    format!("{n} track{}", if n == 1 { "" } else { "s" })
}

/// The window's fields. A bucket being changed keeps its bucket and prefix (another
/// would be another source), and its key unless a new one is typed.
fn fields(ui: &mut Ui, f: &mut BucketForm, new: bool) {
    let row = |ui: &mut Ui, label: &str, add: &mut dyn FnMut(&mut Ui)| {
        ui.label(label);
        add(ui);
        ui.end_row();
    };
    let text = |ui: &mut Ui, value: &mut String, hint: &str| {
        ui.add(
            TextEdit::singleline(value)
                .hint_text(hint)
                .desired_width(280.0),
        );
    };
    egui::Grid::new("bucket-form")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            row(ui, "Name", &mut |ui| {
                text(ui, &mut f.name, "what to call it here (the bucket's name)")
            });
            row(ui, "Endpoint", &mut |ui| {
                text(
                    ui,
                    &mut f.endpoint,
                    "blank for Amazon S3; else the service's URL",
                )
            });
            row(ui, "Region", &mut |ui| {
                text(ui, &mut f.region, "us-east-1; auto for Cloudflare R2")
            });
            row(ui, "Bucket", &mut |ui| {
                ui.add_enabled_ui(new, |ui| text(ui, &mut f.bucket, "its name"));
            });
            row(ui, "Prefix", &mut |ui| {
                ui.add_enabled_ui(new, |ui| {
                    text(ui, &mut f.prefix, "a folder in it (optional)")
                });
            });
            row(ui, "Access key ID", &mut |ui| {
                text(ui, &mut f.access_key_id, if new { "" } else { "unchanged" })
            });
            row(ui, "Secret access key", &mut |ui| {
                ui.add(
                    TextEdit::singleline(&mut f.secret_access_key)
                        .password(true)
                        .hint_text(if new { "" } else { "unchanged" })
                        .desired_width(280.0),
                );
            });
        });
    muted(
        ui,
        "Cloudflare R2's endpoint is https://<account id>.r2.cloudflarestorage.com.",
    );
}
