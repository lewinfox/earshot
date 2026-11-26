//! Bucket sources through the app (`core::bucket`), against a stand-in S3 service:
//! one is added, its tracks are listed without being downloaded, a track is
//! downloaded when it's loaded, and a crate's are when it's kept offline.

use earshot_engine::media::Location;
use egui_kittest::kittest::Queryable;

use super::*;
use crate::core::bucket::tests::fake;
use crate::core::bucket::{BucketCmd, BucketForm};
use crate::core::collections::CollectionKind;
use crate::core::sources::SourceKind;
use std::collections::BTreeSet;

/// The tracks that are a bucket's: (id, URL), in path order.
fn in_buckets(a: &EarshotApp) -> Vec<(i64, String)> {
    a.index
        .tracks()
        .iter()
        .filter(|t| t.path.starts_with("s3://"))
        .map(|t| (t.id, t.path.clone()))
        .collect()
}

/// `audio` (a WAV) with a few of its last samples changed by `name`: other audio,
/// as far as the audio hash goes, that still plays. (The fixture's own would be the
/// same track as the one in the music folder.)
fn other_audio(audio: &[u8], name: &str) -> Vec<u8> {
    let mut out = audio.to_vec();
    let n = out.len();
    for (i, b) in name.bytes().enumerate() {
        out[n - 2000 + 2 * i] ^= b | 1;
    }
    out
}

fn downloaded(url: &str) -> bool {
    Location::Url(url.into()).here().is_some()
}

#[test]
fn a_bucket_is_added_browsed_and_downloaded_from_when_a_track_is_loaded() {
    let mut h = start("bucket");
    let service = fake::Service::start("test-app-bucket");
    let audio = std::fs::read(fixture_tracks().join("Night Bus.wav")).unwrap();
    for key in [
        "sets/Cloud One.wav",
        "sets/Cloud Two.wav",
        "sets/Cloud Three.wav",
    ] {
        service.put(key, &other_audio(&audio, key));
    }
    let form = BucketForm {
        name: "Gig box".into(),
        endpoint: service.endpoint.clone(),
        bucket: service.bucket.clone(),
        prefix: "sets".into(),
        access_key_id: fake::KEY_ID.into(),
        secret_access_key: fake::SECRET.into(),
        ..Default::default()
    };

    // A key the service doesn't accept: nothing is added, and the window would say why.
    let wrong = BucketForm {
        access_key_id: "AKIAWRONG".into(),
        ..form.clone()
    };
    h.state_mut().dispatch(
        BucketCmd::Save {
            form: wrong,
            source: None,
        }
        .into(),
    );
    settle_until(&mut h, |a| !a.sources.iter().any(|s| s.kind.is_bucket()));

    // The right one: it's a source under its name, and its tracks are in the library
    // beside the folder's, with nothing downloaded and nothing analysed.
    h.state_mut()
        .dispatch(BucketCmd::Save { form, source: None }.into());
    settle_until(&mut h, |a| in_buckets(a).len() == 3);
    let a = h.state();
    let source = a
        .sources
        .iter()
        .find(|s| s.kind == SourceKind::S3)
        .expect("the bucket's source");
    assert_eq!(source.name, "Gig box");
    assert_eq!(a.index.tracks().len(), TRACKS.len() + 3);
    let cloud = in_buckets(a);
    assert_eq!(
        cloud[0].1,
        format!("s3://{}/sets/Cloud One.wav", service.bucket)
    );
    for (id, url) in &cloud {
        assert!(!downloaded(url), "{url} was downloaded by finding it");
        let t = a.index.get(*id).unwrap();
        assert!(t.bpm.is_none(), "{url} was analysed");
        assert!(t.title.starts_with("Cloud"));
    }
    assert!(
        service
            .asked()
            .iter()
            .all(|r| r.what == "LIST" || r.range.is_some()),
        "only listed, and read by ranges"
    );

    // It has an icon of its own in the left stack ("GB"), and its pane is open: its
    // name heads the pane and the list (not "sets", its prefix), which lists its
    // tracks.
    use crate::core::view::Source as Pane;
    let pane = Pane::Bucket(source.id);
    assert_eq!(h.state().prefs.layout().source, Some(pane));
    steps(&mut h, 3);
    let icons = |a: &EarshotApp| -> Vec<(Pane, String)> {
        let items = a.panels.library.buckets.stack_items();
        items.iter().map(|i| (i.id, i.icon.to_string())).collect()
    };
    assert_eq!(icons(h.state()), [(pane, "GB".to_string())]);
    assert_eq!(
        h.query_all_by_label("Gig box").count(),
        2,
        "the pane and the list"
    );
    assert!(h.query_by_label("sets").is_none());
    assert_eq!(h.state().panels.library.browser.shown().len(), 3);
    h.get_by_label("All tracks  (3)");
    // Another view, then its icon again: back on its tracks.
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Pane::AllTracks)).into());
    steps(&mut h, 2);
    h.get_by_label_contains("All tracks  (").click();
    steps(&mut h, 2);
    assert_eq!(
        h.state().panels.library.browser.shown().len(),
        TRACKS.len() + 3
    );
    // The library's Storage tree has it too, after Local: picking it there lists its
    // tracks, the library's pane still open.
    h.get_by_label(&format!("Local  ({})", TRACKS.len()));
    h.get_by_label("Gig box  (3)").click();
    steps(&mut h, 3);
    assert_eq!(h.state().prefs.layout().source, Some(Pane::AllTracks));
    assert_eq!(h.state().panels.library.browser.shown().len(), 3);
    render(&mut h, "bucket-in-storage");
    h.state_mut().dispatch(ViewCmd::Source(Some(pane)).into());
    steps(&mut h, 3);
    assert_eq!(h.state().prefs.layout().source, Some(pane));
    assert_eq!(h.state().panels.library.browser.shown().len(), 3);

    // The + pane only adds sources: the bucket isn't listed there; its button opens
    // the window that adds another.
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(Pane::Manage)).into());
    steps(&mut h, 3);
    h.get_by_label("Add a bucket…").click();
    steps(&mut h, 3);
    assert!(h.state().panels.library.buckets.dialog_open());
    render(&mut h, "bucket-window");
    h.get_by_label("Cancel").click();
    steps(&mut h, 2);
    assert!(!h.state().panels.library.buckets.dialog_open());

    // Loaded: downloaded first, then on the deck like any file.
    let (one, two, three) = (cloud[0].clone(), cloud[1].clone(), cloud[2].clone());
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(one.0).into());
    settle_until(&mut h, |a| prepared(a) == Some(one.0));
    assert!(downloaded(&one.1));
    assert!(!downloaded(&two.1) && !downloaded(&three.1));
    assert!(h.state().buckets.cache.usage() >= audio.len() as u64);

    // Analysis always looks for the key: the folder's tracks, analysed at the start,
    // have the key analyser's result.
    {
        let a = h.state();
        let local = a
            .index
            .tracks()
            .iter()
            .find(|t| !t.path.starts_with("s3://"))
            .unwrap();
        let ran = a.lib.store.reader.analysis_results(local.id).unwrap();
        assert!(ran.iter().any(|r| r.analyser == "key"), "{ran:?}");
    }
    // With a bucket, All tracks has the download column too (put in once).
    {
        use crate::ui::panels::library::table::{ColumnId, TableView};
        let all = h
            .state_mut()
            .panels
            .library
            .tables
            .get_mut(TableView::Library);
        assert!(all.columns.iter().any(|c| c.0 == ColumnId::Download));
        assert!(all.download_offered);
    }

    // The list's download column knows which is which; a local track has no mark.
    let marks = |a: &EarshotApp| [one.0, two.0, three.0].map(|id| a.index.downloaded(id));
    assert_eq!(marks(h.state()), [Some(true), Some(false), Some(false)]);
    let local = ids(&h)
        .into_iter()
        .find(|id| ![one.0, two.0, three.0].contains(id))
        .unwrap();
    assert_eq!(h.state().index.downloaded(local), None);

    // The computer offline: the bucket tracks that aren't downloaded leave the lists
    // (they couldn't be loaded); the downloaded one and the folder's stay.
    let listed = |a: &EarshotApp| {
        let all = crate::core::query::parse("").unwrap();
        a.index.query(&all, &a.collections, None)
    };
    assert_eq!(listed(h.state()).len(), TRACKS.len() + 3);
    service.asked();
    h.state_mut().set_online(false);
    assert!(h.state().index.is_hidden(two.0) && h.state().index.is_hidden(three.0));
    assert!(!h.state().index.is_hidden(one.0));
    let now = listed(h.state());
    assert_eq!(now.len(), TRACKS.len() + 1);
    assert!(now.contains(&one.0) && now.contains(&local));
    // The list says how many it leaves out; the Sources pane, why.
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Perform).into());
    h.state_mut().dispatch(ViewCmd::Source(Some(pane)).into());
    steps(&mut h, 2);
    h.get_by_label("☁ 2 hidden");
    h.get_by_label_contains("This computer is offline");
    render(&mut h, "bucket-offline");
    // Nothing was asked of the bucket meanwhile.
    settle(&mut h);
    assert_eq!(service.asked(), []);
    // Online again: they're back, and the bucket is read through for changes.
    h.state_mut().set_online(true);
    settle_until(&mut h, |a| a.index.hidden_count() == 0);
    assert_eq!(listed(h.state()).len(), TRACKS.len() + 3);
    assert!(service.asked().iter().any(|r| r.what == "LIST"));

    // A crate kept offline: its bucket tracks are downloaded now, and one added to it
    // later is too.
    h.state_mut().dispatch(
        CollectionCmd::Create {
            kind: CollectionKind::Crate,
            parent: None,
            tracks: vec![two.0],
            query: None,
        }
        .into(),
    );
    settle(&mut h);
    let krate = h
        .state()
        .collections
        .children(None)
        .into_iter()
        .find(|c| c.kind == CollectionKind::Crate)
        .expect("the crate")
        .id;
    h.state_mut().dispatch(
        BucketCmd::KeepOffline {
            collection: krate,
            on: true,
        }
        .into(),
    );
    settle_until(&mut h, |_| downloaded(&two.1) && !downloaded(&three.1));
    h.state_mut().dispatch(
        CollectionCmd::AddTracks {
            to: krate,
            tracks: vec![three.0],
        }
        .into(),
    );
    settle_until(&mut h, |_| downloaded(&three.1));
    assert_eq!(marks(h.state()), [Some(true); 3]);
    // (All three downloaded, in the bucket's own list with its download column.)
    h.state_mut().dispatch(ViewCmd::Source(Some(pane)).into());
    steps(&mut h, 2);
    render(&mut h, "bucket");

    // With the cache's limit below what it holds, what's kept offline stays and the
    // rest goes. (Limits are whole GB in the settings; the cache takes bytes.)
    let cache = h.state().buckets.cache.clone();
    cache.set_limit(1);
    assert!(!downloaded(&one.1));
    assert!(downloaded(&two.1) && downloaded(&three.1));
    h.state_mut().refresh_downloads();
    assert_eq!(marks(h.state()), [Some(false), Some(true), Some(true)]);

    // Removed: its tracks leave the library, and its downloads go.
    let id = h
        .state()
        .sources
        .iter()
        .find(|s| s.kind.is_bucket())
        .unwrap()
        .id;
    h.state_mut().dispatch(LibraryCmd::RemoveSource(id).into());
    settle_until(&mut h, |a| {
        in_buckets(a).is_empty() && a.index.tracks().len() == TRACKS.len()
    });
    // Its icon and pane go with it.
    steps(&mut h, 3);
    assert_eq!(h.state().prefs.layout().source, None);
    assert!(icons(h.state()).is_empty());
    assert!(!cache.path(&two.1).exists());
}

/// A silent AIFF with an ID3 chunk of text `frames` (id, text), as taggers write one:
/// a WAV's tags aren't read (symphonia drops its INFO list).
fn tagged_aiff(frames: &[(&[u8; 4], &str)]) -> Vec<u8> {
    let mut id3 = Vec::new();
    for (id, text) in frames {
        id3.extend(*id);
        id3.extend((1 + text.len() as u32).to_be_bytes());
        id3.extend([0, 0, 3]); // no flags; UTF-8
        id3.extend(text.as_bytes());
    }
    let n = id3.len() as u32;
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    tag.extend([21, 14, 7, 0].map(|s| (n >> s) as u8 & 0x7f));
    tag.extend(id3);
    if tag.len() % 2 == 1 {
        tag.push(0);
    }
    let frames = 4410u32;
    let data = vec![0u8; frames as usize * 4];
    let mut b = b"FORM\0\0\0\0AIFFCOMM".to_vec();
    b.extend(18u32.to_be_bytes());
    b.extend(2u16.to_be_bytes());
    b.extend(frames.to_be_bytes());
    b.extend(16u16.to_be_bytes());
    b.extend([0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]); // 44100
    b.extend(b"SSND");
    b.extend((8 + data.len() as u32).to_be_bytes());
    b.extend([0; 8]);
    b.extend(data);
    b.extend(b"ID3 ");
    b.extend((tag.len() as u32).to_be_bytes());
    b.extend(tag);
    let form = (b.len() - 8) as u32;
    b[4..8].copy_from_slice(&form.to_be_bytes());
    b
}

#[test]
fn a_buckets_pane_filters_its_tracks_by_artist_and_genre() {
    let mut h = start("bucket-filters");
    let service = fake::Service::start("test-app-bucket-filters");
    for (key, artist, genre) in [
        ("One.aiff", "Daft", "House; Techno"),
        ("Two.aiff", "Daft", "House"),
        ("deep/Three.aiff", "Someone", "Ambient"),
    ] {
        let title = key.rsplit('/').next().unwrap().trim_end_matches(".aiff");
        let file = tagged_aiff(&[(b"TIT2", title), (b"TPE1", artist), (b"TCON", genre)]);
        service.put(key, &file);
    }
    let form = BucketForm {
        name: "Crate box".into(),
        endpoint: service.endpoint.clone(),
        bucket: service.bucket.clone(),
        access_key_id: fake::KEY_ID.into(),
        secret_access_key: fake::SECRET.into(),
        ..Default::default()
    };
    h.state_mut()
        .dispatch(BucketCmd::Save { form, source: None }.into());
    settle_until(&mut h, |a| in_buckets(a).len() == 3);
    steps(&mut h, 3);
    let shown = |h: &H| -> Vec<String> {
        let a = h.state();
        let mut v: Vec<String> = (a.panels.library.browser.shown().iter())
            .map(|id| a.index.get(*id).unwrap().title.clone())
            .collect();
        v.sort();
        v
    };
    assert_eq!(shown(&h), ["One", "Three", "Two"]);
    // Genre is a column of a bucket's list, read from the tags.
    h.get_by_label("House; Techno");

    let filters = |h: &H| {
        use crate::ui::panels::library::table::{ColumnId, TableView};
        let t = &h.state().panels.library.tables.0[&TableView::Bucket];
        (t.values(ColumnId::Artist), t.values(ColumnId::Genre))
    };
    let none = || -> (BTreeSet<String>, BTreeSet<String>) { Default::default() };

    // The tree: all its tracks, then its artists, genres and folders, closed.
    h.get_by_label("All tracks  (3)");
    assert!(h.query_by_label("House  (2)").is_none());

    // A genre: its tracks, including one with it among others. Each genre with how
    // many tracks have it.
    h.get_by_label("Genres  (3)").click();
    steps(&mut h, 2);
    h.get_by_label("Techno  (1)");
    h.get_by_label("Ambient  (1)");
    h.get_by_label("House  (2)").click();
    steps(&mut h, 3);
    assert_eq!(shown(&h), ["One", "Two"]);
    render(&mut h, "bucket-tree");

    // An artist instead: one row picked at a time, so the genre goes.
    h.get_by_label("Artists  (2)").click();
    steps(&mut h, 2);
    h.get_by_label("Someone  (1)").click();
    steps(&mut h, 3);
    assert_eq!(shown(&h), ["Three"]);
    assert_eq!(
        filters(&h),
        (["Someone".to_string()].into(), Default::default())
    );

    // A folder: its tracks, by no artist or genre.
    h.get_by_label("Folders").click();
    steps(&mut h, 2);
    h.get_by_label("deep").click();
    steps(&mut h, 3);
    assert_eq!(shown(&h), ["Three"]);
    assert_eq!(filters(&h), none());
    h.get_by_label("Daft  (2)").click();
    steps(&mut h, 3);
    assert_eq!(shown(&h), ["One", "Two"]);

    // All its tracks again.
    h.get_by_label("All tracks  (3)").click();
    steps(&mut h, 3);
    assert_eq!(shown(&h), ["One", "Three", "Two"]);
    assert_eq!(filters(&h), none());
}

/// The same audio in the music folder and in a bucket: two rows until the bucket's
/// copy has been downloaded once (its audio isn't known before), then one track,
/// played from the local file, with the download dropped from the cache at once.
#[test]
fn the_same_track_in_a_folder_and_a_bucket_is_one_track() {
    let mut h = start("bucket-twin");
    let service = fake::Service::start("test-app-twin");
    // The folder's own "Night Bus", byte for byte, and one that's only in the bucket.
    let audio = std::fs::read(fixture_tracks().join("Night Bus.wav")).unwrap();
    service.put("sets/Night Bus (cloud).wav", &audio);
    service.put("sets/Only Here.wav", &other_audio(&audio, "only"));
    let form = BucketForm {
        name: "Gig box".into(),
        endpoint: service.endpoint.clone(),
        bucket: service.bucket.clone(),
        prefix: "sets".into(),
        access_key_id: fake::KEY_ID.into(),
        secret_access_key: fake::SECRET.into(),
        ..Default::default()
    };
    h.state_mut()
        .dispatch(BucketCmd::Save { form, source: None }.into());
    settle_until(&mut h, |a| in_buckets(a).len() == 2);
    assert_eq!(
        h.state().index.tracks().len(),
        TRACKS.len() + 2,
        "shown twice"
    );
    let local = h
        .state()
        .index
        .tracks()
        .iter()
        .find(|t| t.title == "Night Bus")
        .map(|t| (t.id, t.path.clone(), t.bpm))
        .unwrap();
    assert!(local.2.is_some(), "the folder's is analysed");
    let cloud = in_buckets(h.state())
        .into_iter()
        .find(|t| t.1.contains("Night Bus"))
        .unwrap();
    // The folder's track is in a crate.
    h.state_mut().dispatch(
        CollectionCmd::Create {
            kind: CollectionKind::Crate,
            parent: None,
            tracks: vec![local.0],
            query: None,
        }
        .into(),
    );
    settle(&mut h);

    // The bucket's copy loaded: downloaded, hashed, and found to be the folder's.
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(cloud.0).into());
    settle_until(&mut h, |a| a.index.tracks().len() == TRACKS.len() + 1);
    let a = h.state();
    // One track, the one on the deck: at the folder's file, with its analysis, in its
    // crate, and still in the bucket's list.
    assert_eq!(prepared(a), Some(cloud.0));
    assert!(
        a.index.get(local.0).is_none(),
        "the folder's row is this one now"
    );
    let t = a.index.get(cloud.0).unwrap();
    assert_eq!(t.path, local.1);
    assert_eq!(t.remote.as_deref(), Some(cloud.1.as_str()));
    assert_eq!(t.bpm, local.2);
    let krate = a
        .collections
        .children(None)
        .into_iter()
        .find(|c| c.kind == CollectionKind::Crate)
        .unwrap();
    assert_eq!(krate.tracks, [cloud.0]);
    let root = format!("s3://{}/sets", service.bucket);
    assert!(t.in_folder(&root));
    // Played from the folder from now on: the download is gone already, and it has
    // no download mark (it needs none).
    assert!(!downloaded(&cloud.1), "the cached copy went at once");
    assert_eq!(a.index.downloaded(cloud.0), None);
    assert_eq!(
        a.lib.store.reader.track_location(cloud.0).unwrap(),
        Location::File(local.1.clone().into())
    );
    // Loaded again, nothing is asked of the bucket.
    service.asked();
    h.state_mut().dispatch(LibraryCmd::Load(cloud.0).into());
    settle(&mut h);
    assert_eq!(service.asked(), []);
    assert!(!downloaded(&cloud.1));
    // Offline, it's still listed (the bucket's other track isn't).
    h.state_mut().set_online(false);
    assert!(!h.state().index.is_hidden(cloud.0));
    assert_eq!(h.state().index.hidden_count(), 1);
}
