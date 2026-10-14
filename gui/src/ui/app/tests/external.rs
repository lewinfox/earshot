//! Importing from another library, through the app: what was imported shows in the
//! library and in its playlist.

use egui_kittest::kittest::Queryable;

use super::*;
use crate::core::collections::{CollectionCmd, Section};
use crate::core::external::{
    ImportNode, ImportOptions, ImportSave, ImportTrack, NodeKind, external_key,
};
use crate::core::sources::{NewSource, SourceKind};

/// A playlist whose files are outside the library folders (as another program's
/// usually are) shows its tracks.
#[test]
fn an_imported_playlist_lists_its_tracks() {
    let mut h = start_with(
        "import-playlist",
        crate::core::worker_client::testing::stand_in,
    );
    let elsewhere = scratch("import-playlist-elsewhere");
    let files: Vec<PathBuf> = ["x", "y"]
        .iter()
        .map(|n| {
            let p = elsewhere.join(format!("{n}.wav"));
            write_track(&p, 120.0, 4.0);
            p
        })
        .collect();
    let store = h.state().lib.store.clone();
    let lib = store
        .call(|w| {
            w.add_source(&NewSource {
                kind: SourceKind::Mixxx,
                name: "Mixxx".into(),
                location: Some("/test/mixxxdb.sqlite".into()),
                config: Default::default(),
            })
        })
        .unwrap();
    let save = ImportSave {
        source: lib,
        options: ImportOptions::default(),
        files: crate::core::library::changed_files(&store, &files).unwrap(),
        tracks: files
            .iter()
            .enumerate()
            .map(|(i, f)| ImportTrack {
                external_id: i.to_string(),
                file: f.clone(),
                meta: vec![],
                rating: None,
                colour: None,
                grid: vec![],
                hot_cues: vec![],
                memory_cues: vec![],
            })
            .collect(),
        roots: vec![(
            Section::Playlists,
            ImportNode {
                name: "Mixxx".into(),
                kind: NodeKind::Folder,
                external: external_key(lib, Section::Playlists, &[], NodeKind::Folder),
                children: vec![ImportNode {
                    name: "set".into(),
                    kind: NodeKind::Playlist,
                    external: external_key(lib, Section::Playlists, &["set"], NodeKind::Playlist),
                    children: vec![],
                    tracks: vec!["0".into(), "1".into()],
                }],
                tracks: vec![],
            },
        )],
    };
    // No library folders at all, as on a new install.
    let music = h.state().panels.library.browser.folders[0].clone();
    h.state_mut().dispatch(LibraryCmd::RemoveRoot(music).into());
    settle_until(&mut h, |a| a.index.tracks().is_empty());
    let done = store.import(save).unwrap();
    assert_eq!(done.tracks, 2);
    settle_until(&mut h, |a| a.index.tracks().len() == 2);
    let id = h
        .state()
        .collections
        .children_in(Section::Playlists, None)
        .into_iter()
        .flat_map(|f| h.state().collections.children(Some(f.id)))
        .find(|c| c.name == "set")
        .map(|c| c.id)
        .expect("the playlist");
    h.state_mut().dispatch(CollectionCmd::Open(id).into());
    steps(&mut h, 3);
    assert_eq!(h.state().panels.library.browser.shown().len(), 2);
    // Drawn, too: not the "no library folders" message instead.
    assert!(
        h.query_by_label("No library folders yet. Menu > Import > Folder...")
            .is_none()
    );
    assert!(
        h.query_all_by_label("x").next().is_some(),
        "the first track's row"
    );
}
