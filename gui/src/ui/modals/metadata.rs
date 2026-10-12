//! The metadata editor: title, artist, album, genre, comment, key, rating, colour and
//! tags of one track or many. With many, a field the tracks don't agree on shows as
//! mixed, and Save changes only the fields touched. Values are saved as the user's
//! (`track_meta` source 'user'), so a rescan keeps them; nothing is written to the
//! files. Saving is one undo step (`EditCmd::Set`).

use egui::{RichText, Sense, Stroke, Ui, vec2};

use crate::core::command::Command;
use crate::core::edit::{EditCmd, Field, Value};
use crate::core::index::LibraryIndex;
use crate::core::track::TrackId;
use crate::ui::theme::{CUE_PALETTE, THEME, rgb};

/// The window's title, which is also its id.
pub const TITLE: &str = "Edit tracks";

#[derive(Default)]
pub struct MetaEditor {
    tracks: Vec<TrackId>,
    /// The track's title, or how many tracks.
    heading: String,
    rows: Vec<Row>,
}

/// What a field showed when the editor opened.
#[derive(Clone, Debug, PartialEq)]
enum Start {
    Same(Option<Value>),
    /// The tracks have different values.
    Mixed,
}

struct Row {
    field: Field,
    start: Start,
    /// Text fields, the key and the tags (comma-separated).
    text: String,
    rating: Option<u8>,
    colour: Option<u32>,
    /// Changed by the user: only these are saved.
    touched: bool,
}

impl Row {
    fn new(field: Field, start: Start) -> Row {
        let (mut text, mut rating, mut colour) = (String::new(), None, None);
        match &start {
            Start::Same(Some(Value::Text(s))) => text = s.clone(),
            Start::Same(Some(Value::Tags(t))) => text = t.join(", "),
            Start::Same(Some(Value::Rating(r))) => rating = Some(*r),
            Start::Same(Some(Value::Colour(c))) => colour = Some(*c),
            Start::Same(None) | Start::Mixed => {}
        }
        Row {
            field,
            start,
            text,
            rating,
            colour,
            touched: false,
        }
    }

    /// The value to save. Empty text clears the user's value, so the file's tag
    /// shows again.
    fn value(&self) -> Option<Value> {
        match self.field {
            Field::Rating => self.rating.map(Value::Rating),
            Field::Colour => self.colour.map(Value::Colour),
            Field::Tags => match Value::tags(self.text.split(',')) {
                Value::Tags(t) if t.is_empty() => None,
                tags => Some(tags),
            },
            _ => {
                let t = self.text.trim();
                (!t.is_empty()).then(|| Value::Text(t.to_string()))
            }
        }
    }

    fn mixed(&self) -> bool {
        self.start == Start::Mixed && !self.touched
    }
}

impl MetaEditor {
    /// Start editing these tracks, from what the library shows now.
    pub fn open(&mut self, tracks: Vec<TrackId>, lib: &LibraryIndex) {
        let found: Vec<_> = tracks.iter().filter_map(|id| lib.get(*id)).collect();
        self.heading = match found.as_slice() {
            [t] => t.title.clone(),
            _ => format!("{} tracks", found.len()),
        };
        self.rows = Field::ALL
            .iter()
            .map(|&f| {
                let values: Vec<Option<Value>> = found.iter().map(|t| f.get(t)).collect();
                let start = if values.windows(2).all(|w| w[0] == w[1]) {
                    Start::Same(values.into_iter().next().flatten())
                } else {
                    Start::Mixed
                };
                Row::new(f, start)
            })
            .collect();
        self.tracks = found.iter().map(|t| t.id).collect();
    }

    /// What Save sets: the fields touched, and only those.
    fn changes(&self) -> Vec<(Field, Option<Value>)> {
        self.rows
            .iter()
            .filter(|r| r.touched)
            .map(|r| (r.field, r.value()))
            .filter(|(f, v)| {
                // Touched but left as it was: nothing to save.
                let row = self.rows.iter().find(|r| r.field == *f).unwrap();
                row.start != Start::Same(v.clone())
            })
            .collect()
    }

    /// Draw the form. Returns true once it's done with (saved or cancelled).
    pub fn ui(&mut self, ui: &mut Ui, out: &mut Vec<Command>) -> bool {
        ui.label(RichText::new(&self.heading).strong());
        if self.tracks.len() > 1 {
            ui.label(
                RichText::new("Fields marked mixed differ between the tracks; only the ones you change are saved.")
                    .color(THEME.muted),
            );
        }
        ui.add_space(6.0);
        egui::Grid::new("metadata-grid")
            .num_columns(2)
            .spacing(vec2(10.0, 6.0))
            .show(ui, |ui| {
                for row in &mut self.rows {
                    ui.label(row.field.label());
                    match row.field {
                        Field::Rating => rating(ui, row),
                        Field::Colour => colour(ui, row),
                        _ => text(ui, row),
                    }
                    ui.end_row();
                }
            });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Empty text shows the file's own tag. Files aren't changed.")
                .small()
                .color(THEME.muted),
        );
        ui.add_space(6.0);
        let mut done = false;
        ui.horizontal(|ui| {
            let changes = self.changes();
            if ui
                .add_enabled(!changes.is_empty(), egui::Button::new("Save"))
                .clicked()
            {
                out.push(
                    EditCmd::Set {
                        tracks: self.tracks.clone(),
                        values: changes,
                    }
                    .into(),
                );
                done = true;
            }
            if ui.button("Cancel").clicked() {
                done = true;
            }
        });
        done
    }
}

fn text(ui: &mut Ui, row: &mut Row) {
    let hint = match (row.mixed(), row.field) {
        (true, _) => "mixed",
        (false, Field::Tags) => "comma, separated",
        (false, Field::Key) => "8A, Am, F#m …",
        _ => "",
    };
    let edit = egui::TextEdit::singleline(&mut row.text)
        .hint_text(hint)
        .desired_width(320.0);
    if ui.add(edit).changed() {
        row.touched = true;
    }
}

fn rating(ui: &mut Ui, row: &mut Row) {
    ui.horizontal(|ui| {
        for n in 1..=5u8 {
            let on = row.rating.is_some_and(|r| r >= n) && !row.mixed();
            if ui.selectable_label(on, n.to_string()).clicked() {
                row.rating = Some(n);
                row.touched = true;
            }
        }
        if ui.small_button("none").clicked() {
            row.rating = None;
            row.touched = true;
        }
        if row.mixed() {
            ui.label(RichText::new("mixed").color(THEME.muted));
        }
    });
}

fn colour(ui: &mut Ui, row: &mut Row) {
    ui.horizontal(|ui| {
        for (c, name) in CUE_PALETTE {
            let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
            ui.painter().rect_filled(rect, 3.0, rgb(c));
            if row.colour == Some(c) && !row.mixed() {
                ui.painter().rect_stroke(
                    rect.expand(2.0),
                    3.0,
                    Stroke::new(1.5_f32, THEME.text),
                    egui::StrokeKind::Outside,
                );
            }
            if resp.on_hover_text(name).clicked() {
                row.colour = Some(c);
                row.touched = true;
            }
        }
        if ui.small_button("none").clicked() {
            row.colour = None;
            row.touched = true;
        }
        if row.mixed() {
            ui.label(RichText::new("mixed").color(THEME.muted));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::track::lib_track;

    fn lib() -> LibraryIndex {
        let mut a = lib_track(1, "A", "Artist A");
        a.genre = "House".into();
        a.rating = Some(3);
        let mut b = lib_track(2, "B", "Artist B");
        b.genre = "House".into();
        b.tags = vec!["dub".into()];
        let mut ix = LibraryIndex::default();
        ix.replace(vec![a, b]);
        ix
    }

    fn row(e: &mut MetaEditor, f: Field) -> &mut Row {
        e.rows.iter_mut().find(|r| r.field == f).unwrap()
    }

    #[test]
    fn mixed_fields_show_as_mixed_and_only_touched_ones_save() {
        let mut e = MetaEditor::default();
        e.open(vec![1, 2, 99], &lib());
        assert_eq!(e.tracks, vec![1, 2], "a track that's gone is left out");
        assert_eq!(e.heading, "2 tracks");
        assert_eq!(row(&mut e, Field::Artist).start, Start::Mixed);
        assert_eq!(row(&mut e, Field::Rating).start, Start::Mixed);
        assert_eq!(row(&mut e, Field::Tags).start, Start::Mixed);
        let genre = row(&mut e, Field::Genre);
        assert_eq!(genre.start, Start::Same(Some(Value::Text("House".into()))));
        assert_eq!(genre.text, "House");
        assert!(e.changes().is_empty(), "nothing touched");

        // Change the genre and set a rating: the mixed artists are left alone.
        let genre = row(&mut e, Field::Genre);
        genre.text = " Disco ".into();
        genre.touched = true;
        let r = row(&mut e, Field::Rating);
        r.rating = Some(5);
        r.touched = true;
        // Touched, but typed back to what it was: not saved.
        let album = row(&mut e, Field::Album);
        album.text = String::new();
        album.touched = true;
        assert_eq!(
            e.changes(),
            vec![
                (Field::Genre, Some(Value::Text("Disco".into()))),
                (Field::Rating, Some(Value::Rating(5))),
            ]
        );

        // Tags as typed: one list for every track.
        let tags = row(&mut e, Field::Tags);
        tags.text = "warm, dub,, warm".into();
        tags.touched = true;
        assert!(
            e.changes()
                .contains(&(Field::Tags, Some(Value::tags(["dub", "warm"]))))
        );
    }

    #[test]
    fn one_track_shows_its_values() {
        let mut e = MetaEditor::default();
        e.open(vec![1], &lib());
        assert_eq!(e.heading, "A");
        assert_eq!(row(&mut e, Field::Artist).text, "Artist A");
        assert_eq!(row(&mut e, Field::Rating).rating, Some(3));
        // Emptying the title clears the user's value (the tag's shows again).
        let t = row(&mut e, Field::Title);
        t.text = "  ".into();
        t.touched = true;
        assert_eq!(e.changes(), vec![(Field::Title, None)]);
    }
}
