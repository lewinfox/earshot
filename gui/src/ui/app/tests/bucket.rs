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

    // The Sources pane lists it under its name; the window that adds another opens.
    h.state_mut()
        .dispatch(ViewCmd::Source(Some(crate::core::view::Source::Manage)).into());
    h.state_mut().panels.library.buckets.add();
    steps(&mut h, 3);
    assert!(h.state().panels.library.buckets.dialog_open());
    h.get_by_label("Gig box");
    h.get_by_label("Add a bucket…");
    // Picked, its tracks are what's listed, under its name (not "sets", its prefix).
    h.get_by_label("Gig box").click();
    steps(&mut h, 2);
    assert_eq!(
        h.query_all_by_label("Gig box").count(),
        2,
        "the pane's row and the heading"
    );
    assert!(h.query_by_label("sets").is_none());
    assert_eq!(h.state().panels.library.browser.shown().len(), 3);
    render(&mut h, "bucket-window");
    h.get_by_label("Cancel").click();
    steps(&mut h, 2);
    assert!(!h.state().panels.library.buckets.dialog_open());
    render(&mut h, "bucket");

    // Loaded: downloaded first, then on the deck like any file.
    let (one, two, three) = (cloud[0].clone(), cloud[1].clone(), cloud[2].clone());
    h.state_mut()
        .dispatch(ViewCmd::SetMode(Mode::Prepare).into());
    h.state_mut().dispatch(LibraryCmd::Load(one.0).into());
    settle_until(&mut h, |a| prepared(a) == Some(one.0));
    assert!(downloaded(&one.1));
    assert!(!downloaded(&two.1) && !downloaded(&three.1));
    assert!(h.state().buckets.cache.usage() >= audio.len() as u64);

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

    // With the cache's limit below what it holds, what's kept offline stays and the
    // rest goes. (Limits are whole GB in the settings; the cache takes bytes.)
    let cache = h.state().buckets.cache.clone();
    cache.set_limit(1);
    assert!(!downloaded(&one.1));
    assert!(downloaded(&two.1) && downloaded(&three.1));

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
    assert!(!cache.path(&two.1).exists());
}
