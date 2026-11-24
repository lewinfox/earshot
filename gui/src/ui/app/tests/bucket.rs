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

/// The tracks that are a bucket's: (id, URL), in path order.
fn in_buckets(a: &EarshotApp) -> Vec<(i64, String)> {
    a.index
        .tracks()
        .iter()
        .filter(|t| t.path.starts_with("s3://"))
        .map(|t| (t.id, t.path.clone()))
        .collect()
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
        service.put(key, &audio);
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
    h.get_by_label("All of it  (3)");
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
