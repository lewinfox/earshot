//! Bucket sources in the Sources pane (`core::bucket`): the S3-compatible buckets
//! added, each under the name the user gave it, and the window that adds one (or
//! changes one's name, service or key).

use std::collections::{HashMap, HashSet};

use egui::{RichText, TextEdit, Ui};

use super::browser::Scope;
use crate::core::bucket::{BucketCmd, BucketForm, Scan};
use crate::core::command::Command;
use crate::core::index::LibraryCmd;
use crate::core::sources::{Source, SourceId};
use crate::ui::modals::show_fitted;
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

    /// The Sources pane's Buckets section: each bucket (picking one lists its tracks
    /// in the middle), and the button that adds one.
    /// Returns the bucket picked, as the scope that lists its tracks.
    pub fn manage(&mut self, ui: &mut Ui, scope: &Scope, out: &mut Vec<Command>) -> Option<Scope> {
        let mut picked = None;
        if self.sources.is_empty() {
            muted(ui, "No buckets yet.");
        } else if self.offline {
            ui.add(
                Note::warning(
                    "This computer is offline: only the tracks already downloaded are \
                     listed. The rest are back when it's online.",
                )
                .small(),
            );
        }
        for s in self.sources.clone() {
            let Some(url) = s.bucket_url() else { continue };
            let selected = matches!(scope, Scope::Folder(f) if *f == url);
            let endpoint = s
                .config
                .s3
                .as_ref()
                .and_then(|c| c.endpoint.clone())
                .unwrap_or_else(|| "Amazon S3".into());
            let r = ui
                .selectable_label(selected, RichText::new(&s.name).strong())
                .on_hover_text(format!("{url}\n{endpoint}"));
            if r.clicked() {
                picked = Some(Scope::Folder(url.clone()));
            }
            r.context_menu(|ui| {
                if ui.button("Read again").clicked() {
                    out.push(BucketCmd::Scan(s.id).into());
                    ui.close();
                }
                if ui.button("Edit…").clicked() {
                    self.dialog = Some(Dialog {
                        source: Some(s.id),
                        form: BucketForm::of(&s),
                        checking: false,
                        error: None,
                    });
                    ui.close();
                }
                ui.separator();
                if ui
                    .button("Remove")
                    .on_hover_text(
                        "Its tracks leave the library, its downloads are deleted and its \
                         access key is forgotten. Nothing in the bucket is touched.",
                    )
                    .clicked()
                {
                    out.push(LibraryCmd::RemoveSource(s.id).into());
                    ui.close();
                }
            });
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
        }
        if ui
            .button("Add a bucket…")
            .on_hover_text("Amazon S3, Cloudflare R2, Backblaze B2, Wasabi, MinIO")
            .clicked()
        {
            self.add();
        }
        picked
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
