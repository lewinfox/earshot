//! Buckets, against a stand-in S3 service on this machine ([`fake::Service`]): it
//! answers the two requests earshot makes (listing a bucket, getting an object or a
//! range of one) and keeps a list of what it was asked.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use earshot_engine::media::Location;

use super::*;
use crate::core::sources::SourceKind;
use crate::core::store::StoreEvent;

/// A stand-in S3 service: one bucket's objects, served over HTTP on a local port.
pub(crate) mod fake {
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// A request it answered.
    #[derive(Clone, Debug, PartialEq)]
    pub struct Asked {
        /// "LIST", or "GET" of an object.
        pub what: &'static str,
        /// The prefix listed, or the object's key.
        pub key: String,
        /// The `Range` asked for, if one was.
        pub range: Option<(u64, u64)>,
    }

    #[derive(Default)]
    struct State {
        /// Key → (bytes, ETag).
        objects: BTreeMap<String, (Vec<u8>, String)>,
        asked: Vec<Asked>,
        /// Answer every request with an error.
        down: bool,
    }

    /// The access key it accepts.
    pub const KEY_ID: &str = "AKIAEARSHOTTEST";
    pub const SECRET: &str = "not-a-real-secret";

    #[derive(Clone)]
    pub struct Service {
        pub bucket: String,
        pub endpoint: String,
        state: Arc<Mutex<State>>,
    }

    impl Service {
        /// Start one, serving a bucket called `bucket`, until the test process ends.
        pub fn start(bucket: &str) -> Service {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let service = Service {
                bucket: bucket.to_string(),
                endpoint: format!("http://{}", listener.local_addr().unwrap()),
                state: Arc::default(),
            };
            let served = service.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let served = served.clone();
                    std::thread::spawn(move || {
                        let _ = served.serve(stream);
                    });
                }
            });
            service
        }

        /// Put an object in the bucket (or replace it: its ETag changes with its bytes).
        pub fn put(&self, key: &str, bytes: &[u8]) {
            use sha2::{Digest, Sha256};
            let digest: [u8; 32] = Sha256::digest(bytes).into();
            let etag = format!("\"{}\"", &crate::core::hash::to_hex(&digest)[..32]);
            self.state
                .lock()
                .unwrap()
                .objects
                .insert(key.to_string(), (bytes.to_vec(), etag));
        }

        pub fn delete(&self, key: &str) {
            self.state.lock().unwrap().objects.remove(key);
        }

        /// Answer every request with an error from now on (or properly again).
        pub fn set_down(&self, down: bool) {
            self.state.lock().unwrap().down = down;
        }

        /// What it's been asked since the last call.
        pub fn asked(&self) -> Vec<Asked> {
            std::mem::take(&mut self.state.lock().unwrap().asked)
        }

        fn serve(&self, stream: TcpStream) -> std::io::Result<()> {
            let mut reader = BufReader::new(stream.try_clone()?);
            let mut stream = stream;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line)? == 0 {
                    return Ok(());
                }
                let mut parts = line.split_whitespace();
                let (method, target) = (
                    parts.next().unwrap_or("").to_string(),
                    parts.next().unwrap_or("").to_string(),
                );
                let (mut range, mut auth, mut length) = (None, String::new(), 0usize);
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h)?;
                    let h = h.trim_end();
                    if h.is_empty() {
                        break;
                    }
                    let Some((name, value)) = h.split_once(':') else {
                        continue;
                    };
                    let value = value.trim();
                    match name.to_ascii_lowercase().as_str() {
                        "range" => {
                            range = value.strip_prefix("bytes=").and_then(|r| {
                                let (a, b) = r.split_once('-')?;
                                Some((a.parse().ok()?, b.parse().ok()?))
                            })
                        }
                        "authorization" => auth = value.to_string(),
                        "content-length" => length = value.parse().unwrap_or(0),
                        _ => {}
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body)?;
                let (status, headers, body) = self.answer(&method, &target, range, &auth);
                let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
                for (k, v) in headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                stream.write_all(out.as_bytes())?;
                if method != "HEAD" {
                    stream.write_all(&body)?;
                }
                stream.flush()?;
            }
        }

        fn answer(
            &self,
            method: &str,
            target: &str,
            range: Option<(u64, u64)>,
            auth: &str,
        ) -> (&'static str, Vec<(&'static str, String)>, Vec<u8>) {
            let error = |status: &'static str, code: &str| {
                let xml = format!(
                    "<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>{code}</Message></Error>"
                );
                (
                    status,
                    vec![("Content-Type", "application/xml".to_string())],
                    xml.into_bytes(),
                )
            };
            let mut state = self.state.lock().unwrap();
            if state.down {
                return error("400 Bad Request", "RequestTimeout");
            }
            if !auth.contains(&format!("Credential={KEY_ID}/")) {
                return error("403 Forbidden", "InvalidAccessKeyId");
            }
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            let path = decode(path);
            let Some(rest) = path.strip_prefix(&format!("/{}", self.bucket)) else {
                return error("404 Not Found", "NoSuchBucket");
            };
            let key = rest.trim_start_matches('/');
            if key.is_empty() {
                // ListObjectsV2.
                let prefix = query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("prefix="))
                    .map(decode)
                    .unwrap_or_default();
                state.asked.push(Asked {
                    what: "LIST",
                    key: prefix.clone(),
                    range: None,
                });
                let mut xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult>\
                     <Name>{}</Name><Prefix>{}</Prefix><MaxKeys>1000</MaxKeys>\
                     <IsTruncated>false</IsTruncated>",
                    self.bucket,
                    escape(&prefix)
                );
                for (key, (bytes, etag)) in state.objects.iter() {
                    if key.starts_with(&prefix) {
                        xml.push_str(&format!(
                            "<Contents><Key>{}</Key>\
                             <LastModified>2026-01-02T03:04:05.000Z</LastModified>\
                             <ETag>{}</ETag><Size>{}</Size>\
                             <StorageClass>STANDARD</StorageClass></Contents>",
                            escape(key),
                            escape(etag),
                            bytes.len()
                        ));
                    }
                }
                xml.push_str("</ListBucketResult>");
                return (
                    "200 OK",
                    vec![("Content-Type", "application/xml".to_string())],
                    xml.into_bytes(),
                );
            }
            if method == "GET" {
                state.asked.push(Asked {
                    what: "GET",
                    key: key.to_string(),
                    range,
                });
            }
            let Some((bytes, etag)) = state.objects.get(key) else {
                return error("404 Not Found", "NoSuchKey");
            };
            let mut headers = vec![
                ("ETag", etag.clone()),
                ("Last-Modified", "Fri, 02 Jan 2026 03:04:05 GMT".to_string()),
                ("Accept-Ranges", "bytes".to_string()),
            ];
            match range {
                Some((a, _)) if a as usize >= bytes.len() => {
                    error("416 Range Not Satisfiable", "InvalidRange")
                }
                Some((a, b)) => {
                    let end = (b as usize).min(bytes.len() - 1);
                    headers.push(("Content-Range", format!("bytes {a}-{end}/{}", bytes.len())));
                    (
                        "206 Partial Content",
                        headers,
                        bytes[a as usize..=end].to_vec(),
                    )
                }
                None => ("200 OK", headers, bytes.clone()),
            }
        }
    }

    /// Percent-decoding, as far as a URL's path and query need.
    fn decode(s: &str) -> String {
        let b = s.as_bytes();
        let (mut out, mut i) = (vec![], 0);
        while i < b.len() {
            let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
            match (
                b[i],
                b.get(i + 1).copied().and_then(hex),
                b.get(i + 2).copied().and_then(hex),
            ) {
                (b'%', Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                }
                (c, _, _) => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    fn escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
    }
}

use fake::{Asked, Service};

/// A tagged MP3 (8 KB): "v23_stamped.mp3" has its audio hash in its tags.
fn mp3(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name),
    )
    .unwrap()
}

/// A second of a 440 Hz tone, as a WAV file's bytes: audio that decodes.
pub(crate) fn wav() -> Vec<u8> {
    let rate = 44_100u32;
    let samples: Vec<i16> = (0..rate)
        .map(|i| ((i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 8000.0) as i16)
        .collect();
    let data = samples.len() as u32 * 2;
    let mut out = vec![];
    out.extend(b"RIFF");
    out.extend((36 + data).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data.to_le_bytes());
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    out
}

/// A Store, and the app's buckets, in a scratch folder of this test's own.
struct Rig {
    dir: PathBuf,
    store: Store,
    buckets: Buckets,
    events: Arc<Mutex<Vec<BucketEvent>>>,
    service: Service,
}

impl Rig {
    /// `test` names the scratch folder and the bucket (each test has its own: the
    /// `s3://` remote is shared by the whole process).
    fn new(test: &str) -> Rig {
        // Background work the Store or the buckets start runs when it's started.
        crate::core::background::step_this_thread();
        let dir =
            std::env::temp_dir().join(format!("earshot-bucket-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sink: crate::core::store::EventSink = Arc::new(|_: StoreEvent| {});
        let store = Store::open(&dir.join(crate::core::store::FILE_NAME), sink).unwrap();
        let events: Arc<Mutex<Vec<BucketEvent>>> = Arc::default();
        let tell = events.clone();
        let buckets = Buckets::new(
            &dir,
            1_000_000_000,
            Arc::new(MemorySecrets::default()),
            Jobs::default(),
            Arc::new(move |e| tell.lock().unwrap().push(e)),
        );
        Rig {
            dir,
            store,
            buckets,
            events,
            service: Service::start(&format!("test-{test}")),
        }
    }

    fn form(&self, name: &str, prefix: &str) -> BucketForm {
        BucketForm {
            name: name.into(),
            endpoint: self.service.endpoint.clone(),
            region: String::new(),
            bucket: self.service.bucket.clone(),
            prefix: prefix.into(),
            access_key_id: fake::KEY_ID.into(),
            secret_access_key: fake::SECRET.into(),
        }
    }

    /// Add the service's bucket as a source, as the app does when the form is sent.
    fn add(&self, name: &str, prefix: &str) -> SourceId {
        let id = save(&self.store, &self.buckets, &self.form(name, prefix), None).unwrap();
        self.sync();
        id
    }

    /// As the app does on `StoreEvent::Sources`.
    fn sync(&self) {
        self.buckets
            .set_sources(&self.store.reader.sources().unwrap());
    }

    fn scan(&self, source: SourceId) -> Scan {
        scan(&self.store, &self.buckets, source).unwrap()
    }

    /// The library's tracks, as (path, title).
    fn library(&self) -> Vec<(String, String)> {
        self.store
            .reader
            .library()
            .unwrap()
            .into_iter()
            .map(|t| (t.path, t.title))
            .collect()
    }

    fn url(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.service.bucket)
    }
}

#[test]
fn a_form_says_where_the_bucket_is_and_what_is_wrong_with_it() {
    let mut f = BucketForm {
        bucket: " tunes ".into(),
        prefix: "/house/deep/".into(),
        ..Default::default()
    };
    assert_eq!(f.at().url(), "s3://tunes/house/deep");
    assert_eq!(f.display_name(), "tunes/house/deep");
    // No endpoint: AWS, in its default region. With one: `auto`, and https assumed.
    assert_eq!(
        f.config(),
        S3Config {
            endpoint: None,
            region: "us-east-1".into()
        }
    );
    f.endpoint = "abc.r2.cloudflarestorage.com/".into();
    f.name = "Gig box".into();
    assert_eq!(
        f.config(),
        S3Config {
            endpoint: Some("https://abc.r2.cloudflarestorage.com".into()),
            region: "auto".into()
        }
    );
    assert_eq!(f.display_name(), "Gig box");
    // A new bucket needs its key, both parts; one being changed may keep its key.
    assert!(f.problem(true).is_some());
    assert_eq!(f.problem(false), None);
    f.access_key_id = "id".into();
    assert!(f.problem(false).is_some(), "half a key");
    f.secret_access_key = "secret".into();
    assert_eq!(f.problem(true), None);
    f.bucket = "s3://tunes/house".into();
    assert!(f.problem(true).is_some());
    // The key is never in what's logged.
    assert!(!format!("{f:?}").contains("secret"));
    assert!(!format!("{:?}", f.keys().unwrap()).contains("secret"));
}

#[test]
fn a_bucket_url_splits_into_bucket_and_prefix() {
    let at = BucketAt::parse("s3://tunes/house/deep").unwrap();
    assert_eq!(
        (at.bucket.as_str(), at.prefix.as_str()),
        ("tunes", "house/deep")
    );
    assert_eq!(
        at.object_url("house/deep/a.mp3"),
        "s3://tunes/house/deep/a.mp3"
    );
    assert!(at.holds("house/deep/a.mp3"));
    assert!(!at.holds("house/deeper/a.mp3"), "a prefix is a folder");
    let whole = BucketAt::parse("s3://tunes").unwrap();
    assert_eq!(
        (whole.prefix.as_str(), whole.url().as_str()),
        ("", "s3://tunes")
    );
    assert!(whole.holds("anything.mp3"));
    assert_eq!(BucketAt::parse("/music/s3"), None);
    assert!(is_track("house/A [Original Mix] #1.MP3"));
    assert!(!is_track("house/cover.jpg"));
    assert!(!is_track(".trash/a.mp3"), "hidden");
}

#[test]
fn the_cache_removes_what_was_used_least_recently_but_not_what_is_pinned() {
    let dir = std::env::temp_dir().join(format!("earshot-bucket-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = Cache::new(&dir, 2500);
    let urls: Vec<String> = (0..4)
        .map(|i| format!("s3://b/dir/{i} track.flac"))
        .collect();
    for (i, url) in urls.iter().enumerate() {
        let path = cache.path(url);
        assert_eq!(path.extension().unwrap(), "flac");
        assert!(path.starts_with(dir.join("s3").join("b")));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![0; 1000]).unwrap();
        // Used in this order: 0 longest ago.
        let used = SystemTime::UNIX_EPOCH + Duration::from_secs(1000 + i as u64);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(used)
            .unwrap();
    }
    // A download under way isn't counted out.
    std::fs::write(cache.path(&urls[0]).with_extension("flac.part"), [0; 10]).unwrap();
    // 4010 bytes in 2500: the two oldest that may go, go. Track 0 is pinned.
    cache.set_pinned(HashSet::from([urls[0].clone()]));
    let left = cache.tidy(None);
    let here: Vec<bool> = urls.iter().map(|u| cache.cached(u).is_some()).collect();
    assert_eq!(here, [true, false, false, true]);
    assert_eq!((left, cache.usage()), (2010, 2010));
    // Used again, track 3 would be the last to go; the limit lowered, it's all that
    // can: what's pinned stays, even over the limit.
    cache.set_limit(500);
    assert!(cache.cached(&urls[0]).is_some() && cache.cached(&urls[3]).is_none());
    cache.clear_parts();
    assert_eq!(cache.tidy(None), 1000);
    cache.forget(&urls[0]);
    assert_eq!(cache.usage(), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_bucket_is_listed_and_its_tags_read_without_downloading_it() {
    let rig = Rig::new("scan");
    let s = &rig.service;
    s.put("house/Stamped [v2.3] #1.mp3", &mp3("v23_stamped.mp3"));
    s.put("house/deep/plain.mp3", &mp3("v23_plain.mp3"));
    s.put("house/cover.jpg", b"not audio");
    s.put("techno/other.mp3", &mp3("v24_stamped.mp3"));
    let source = rig.add("Gig box", "house");
    // Saving it listed the prefix once, to check the key.
    assert_eq!(
        s.asked(),
        [Asked {
            what: "LIST",
            key: "house/".into(),
            range: None
        }]
    );
    let saved = rig.store.reader.sources().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        (saved[0].kind, saved[0].name.as_str(), saved[0].bucket_url()),
        (
            SourceKind::S3,
            "Gig box",
            Some(format!("s3://{}/house", s.bucket))
        )
    );

    assert_eq!(
        rig.scan(source),
        Scan {
            tracks: 2,
            read: 2,
            failed: 0
        }
    );
    // Only what's under the prefix, and only audio; untitled files by their names.
    let lib = rig.library();
    assert_eq!(
        lib.iter().map(|t| t.0.clone()).collect::<Vec<_>>(),
        [
            rig.url("house/Stamped [v2.3] #1.mp3"),
            rig.url("house/deep/plain.mp3")
        ]
    );
    // Every read of an object was a range of it: nothing was downloaded whole.
    let asked = s.asked();
    let gets: Vec<&Asked> = asked.iter().filter(|a| a.what == "GET").collect();
    assert!(
        !gets.is_empty() && gets.iter().all(|a| a.range.is_some()),
        "{asked:?}"
    );
    assert!(gets.iter().all(|a| a.key.starts_with("house/")));
    assert!(rig.buckets.cache.cached(&lib[0].0).is_none());
    // The stamped file's audio hash came from its tags, as a local file's would.
    let hashed: i64 = rig
        .store
        .reader
        .with(|c| {
            Ok(c.query_row(
                "SELECT count(*) FROM files WHERE content_hash IS NOT NULL AND etag IS NOT NULL",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(hashed, 1);

    // Again: listed, and nothing read, as no ETag or size changed.
    assert_eq!(
        rig.scan(source),
        Scan {
            tracks: 2,
            read: 0,
            failed: 0
        }
    );
    assert!(s.asked().iter().all(|a| a.what == "LIST"));

    // One replaced, one deleted, one new: the first is read again, the second leaves
    // the library (its track stays in the database), the third joins it.
    s.put("house/deep/plain.mp3", &mp3("v24_stamped.mp3"));
    s.delete("house/Stamped [v2.3] #1.mp3");
    s.put("house/new.mp3", &mp3("v23_plain.mp3"));
    assert_eq!(
        rig.scan(source),
        Scan {
            tracks: 2,
            read: 2,
            failed: 0
        }
    );
    assert_eq!(
        rig.library().into_iter().map(|t| t.0).collect::<Vec<_>>(),
        [rig.url("house/deep/plain.mp3"), rig.url("house/new.mp3")]
    );
}

#[test]
fn opening_a_track_downloads_it_once_and_peeking_does_not() {
    let rig = Rig::new("open");
    let s = &rig.service;
    let bytes = wav();
    s.put("a/one.wav", &bytes);
    let source = rig.add("", "");
    rig.scan(source);
    s.asked();
    let at = Location::Url(rig.url("a/one.wav"));

    // Tags and art: ranges, no local copy.
    assert!(earshot_engine::decode::read_tags(&at).is_ok());
    assert_eq!(at.here(), None);
    assert!(s.asked().iter().all(|a| a.range.is_some()));

    // The whole of it (to decode): downloaded to the cache, in one request.
    let (audio, _) = earshot_engine::decode::decode(&at).unwrap();
    assert!(audio.frames() > 0);
    let copy = at.here().expect("a local copy");
    assert!(copy.starts_with(rig.dir.join("s3")));
    assert_eq!(std::fs::read(&copy).unwrap(), bytes);
    assert_eq!(
        s.asked(),
        [Asked {
            what: "GET",
            key: "a/one.wav".into(),
            range: None
        }]
    );
    assert_eq!(rig.buckets.cache.usage(), bytes.len() as u64);
    assert!(
        rig.events
            .lock()
            .unwrap()
            .contains(&BucketEvent::Downloaded {
                url: rig.url("a/one.wav")
            })
    );

    // From then on nothing is asked of the bucket: not to decode it again, not for
    // its tags, and not with the service gone (a gig with no connection).
    s.set_down(true);
    assert!(earshot_engine::decode::decode(&at).is_ok());
    assert!(earshot_engine::decode::read_tags(&at).is_ok());
    assert_eq!(s.asked(), []);
    // What isn't downloaded can't be read then, and says so.
    s.set_down(false);
    s.put("a/two.wav", &bytes);
    s.set_down(true);
    let e = Location::Url(rig.url("a/two.wav")).open().err().unwrap();
    assert!(e.contains("two.wav"), "{e}");
    s.set_down(false);

    // The object replaced in the bucket: the next scan drops the stale copy.
    s.put("a/one.wav", &wav()[..40_000]);
    rig.scan(source);
    assert_eq!(at.here(), None);
}

#[test]
fn a_wrong_key_is_refused_before_anything_is_saved() {
    let rig = Rig::new("keys");
    let mut form = rig.form("Mine", "");
    form.access_key_id = "AKIAWRONG".into();
    let e = save(&rig.store, &rig.buckets, &form, None).unwrap_err();
    assert!(e.contains("access key"), "{e}");
    assert!(rig.store.reader.sources().unwrap().is_empty());

    // The right one: the source is saved, its key in the keychain and nowhere else.
    let id = rig.add("Mine", "");
    let acct = account(&form.at(), &form.config());
    assert_eq!(
        rig.buckets
            .secrets
            .get(&acct)
            .unwrap()
            .unwrap()
            .access_key_id,
        fake::KEY_ID
    );
    let db = std::fs::read(rig.dir.join(crate::core::store::FILE_NAME)).unwrap();
    let has = |needle: &str| db.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(!has(fake::SECRET) && !has(fake::KEY_ID));

    // Renamed, with the key left blank: the one saved is used.
    let source = rig.store.reader.sources().unwrap().remove(0);
    let mut edit = BucketForm::of(&source);
    assert_eq!(
        (edit.access_key_id.as_str(), edit.bucket.as_str()),
        ("", form.bucket.as_str())
    );
    edit.name = "Renamed".into();
    assert_eq!(save(&rig.store, &rig.buckets, &edit, Some(&source)), Ok(id));
    assert_eq!(rig.store.reader.sources().unwrap()[0].name, "Renamed");

    // Removed: its key is forgotten.
    let source = rig.store.reader.sources().unwrap().remove(0);
    removed(&rig.buckets, &source, &[]);
    assert_eq!(rig.buckets.secrets.get(&acct), Ok(None));
}

#[test]
fn there_can_be_several_buckets_each_with_its_name() {
    let rig = Rig::new("several");
    let s = &rig.service;
    s.put("house/a.mp3", &mp3("v23_plain.mp3"));
    s.put("techno/b.mp3", &mp3("v24_stamped.mp3"));
    let house = rig.add("House", "house");
    let techno = rig.add("Techno", "techno");
    assert_ne!(house, techno);
    // The same bucket and prefix again is the same source, renamed.
    assert_eq!(rig.add("Deep house", "house"), house);
    let names: Vec<String> = rig
        .store
        .reader
        .sources()
        .unwrap()
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(names, ["Deep house", "Techno"]);
    rig.scan(house);
    rig.scan(techno);
    assert_eq!(
        rig.library().into_iter().map(|t| t.0).collect::<Vec<_>>(),
        [rig.url("house/a.mp3"), rig.url("techno/b.mp3")]
    );
    // One removed: its tracks leave the library, the other's stay.
    rig.store.call(move |w| w.remove_source(house)).unwrap();
    rig.sync();
    assert_eq!(
        rig.library().into_iter().map(|t| t.0).collect::<Vec<_>>(),
        [rig.url("techno/b.mp3")]
    );
    // And its objects can't be read any more.
    let e = Location::Url(rig.url("house/a.mp3")).open().err().unwrap();
    assert!(e.contains("isn't one of the sources"), "{e}");

    // A bucket of the same name at another service is refused: its objects would be
    // taken for this one's.
    let elsewhere = Service::start(&s.bucket);
    let mut form = rig.form("Elsewhere", "house");
    form.endpoint = elsewhere.endpoint.clone();
    let e = save(&rig.store, &rig.buckets, &form, None).unwrap_err();
    assert!(e.contains("another service"), "{e}");
}

#[test]
fn objects_the_network_failed_on_are_left_for_the_next_scan() {
    let rig = Rig::new("flaky");
    let s = &rig.service;
    s.put("a.mp3", &mp3("v23_plain.mp3"));
    let source = rig.add("", "");
    // Listed, but every read of an object fails.
    let client = rig.buckets.entry(source).unwrap().client().unwrap();
    let listed = client.list("").unwrap();
    s.set_down(true);
    let at = BucketAt::new(&s.bucket, "");
    assert!(read_object(&client, &at, &listed[0]).is_none());
    s.set_down(false);
    // It isn't recorded untagged: read when the network's back.
    assert!(rig.library().is_empty());
    assert_eq!(rig.scan(source).read, 1);
    assert_eq!(rig.library().len(), 1);
}
