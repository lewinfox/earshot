//! S3-compatible buckets as sources (`SourceKind::S3`): AWS S3, Cloudflare R2,
//! Backblaze B2, Wasabi and MinIO, which all speak the S3 API. There can be any
//! number of them, each with the name the user gave it.
//!
//! - **Listing and tags** ([`scan`]): the bucket's audio files are listed and each
//!   new or changed one (by its ETag and size) has its tags read where it is, a few
//!   ranges of it ([`Ranged`]), so a bucket is browsable without downloading it.
//! - **Audio is never read off the network.** A track that's loaded, previewed or
//!   analysed is downloaded whole to the cache first ([`Cache`]), and everything
//!   downstream reads that file. This is the `s3://` [`Remote`] that
//!   `earshot_audio::media` reads URLs through.
//! - **The cache** is `<cache folder>/s3`, kept under a size limit: over it, the
//!   copies used least recently go ([`Cache::tidy`]), never ones the user asked to
//!   keep ([`Buckets::pin`]).
//! - **Access keys** are in the OS keychain ([`Keychain`]), never in the database.
//!   A read-only key is all that's needed: nothing is ever written to a bucket.
//!
//! A track's file is its object's URL, `s3://<bucket>/<key>`. The bucket's name finds
//! its source ([`Buckets::set_sources`]), and with it the service and the key.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use earshot_engine::decode::{self, TrackTags};
use earshot_engine::media::{self, Location, Media, Remote};
use futures::StreamExt;
use object_store::aws::{AmazonS3, AmazonS3Builder};
use object_store::path::Path as Key;
use object_store::{
    BackoffConfig, ClientOptions, GetOptions, GetRange, ObjectStore, ObjectStoreExt, RetryConfig,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::collections::CollectionId;
use super::hash;
use super::jobs::Jobs;
use super::library;
use super::sources::{S3Config, Source, SourceId};
use super::store::{ScannedObject, Store};
use super::track::TrackId;

/// The scheme of a bucket object's URL.
pub const SCHEME: &str = "s3";

/// The most the cache of downloaded tracks may take, GB, until it's set
/// (`settings::S3_CACHE_GB`).
pub const CACHE_GB_DEFAULT: f64 = 20.0;

// ---- where a bucket is ------------------------------------------------------------------

/// A bucket, or the part of one under a prefix: a bucket source's location,
/// `s3://<bucket>/<prefix>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BucketAt {
    pub bucket: String,
    /// No slash at either end; empty for the whole bucket.
    pub prefix: String,
}

impl BucketAt {
    /// From what the user typed: the prefix's slashes at either end don't count.
    pub fn new(bucket: &str, prefix: &str) -> BucketAt {
        BucketAt {
            bucket: bucket.trim().to_string(),
            prefix: prefix.trim().trim_matches('/').to_string(),
        }
    }

    /// From a source's location, or an object's URL (the key is then the prefix).
    pub fn parse(url: &str) -> Option<BucketAt> {
        let rest = url.strip_prefix("s3://")?;
        let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
        (!bucket.is_empty()).then(|| BucketAt::new(bucket, prefix))
    }

    /// `s3://bucket/prefix`: what its tracks' URLs start with (then a slash).
    pub fn url(&self) -> String {
        match self.prefix.is_empty() {
            true => format!("s3://{}", self.bucket),
            false => format!("s3://{}/{}", self.bucket, self.prefix),
        }
    }

    /// The URL of the object with this key.
    pub fn object_url(&self, key: &str) -> String {
        format!("s3://{}/{key}", self.bucket)
    }

    /// Whether the object with this key is under the prefix.
    fn holds(&self, key: &str) -> bool {
        self.prefix.is_empty()
            || key
                .strip_prefix(&self.prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// An object URL's bucket and key.
fn split_url(url: &str) -> Option<(&str, &str)> {
    let (bucket, key) = url.strip_prefix("s3://")?.split_once('/')?;
    (!bucket.is_empty() && !key.is_empty()).then_some((bucket, key))
}

/// What the user fills in to add a bucket (or change one's name, service or key).
#[derive(Clone, Default, PartialEq, Eq)]
pub struct BucketForm {
    /// What to call it in the library pane. Empty: the bucket's name.
    pub name: String,
    /// The service's URL; empty for AWS.
    pub endpoint: String,
    /// Empty: `us-east-1` for AWS, `auto` elsewhere.
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

impl std::fmt::Debug for BucketForm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key: commands are logged.
        f.debug_struct("BucketForm")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

impl BucketForm {
    /// A source's details, to change them. The key isn't shown: left empty, the one
    /// saved is kept.
    pub fn of(source: &Source) -> BucketForm {
        let at = source.bucket_url().and_then(|u| BucketAt::parse(&u));
        let s3 = source.config.s3.clone().unwrap_or_default();
        BucketForm {
            name: source.name.clone(),
            endpoint: s3.endpoint.unwrap_or_default(),
            region: s3.region,
            bucket: at.as_ref().map(|a| a.bucket.clone()).unwrap_or_default(),
            prefix: at.map(|a| a.prefix).unwrap_or_default(),
            ..Default::default()
        }
    }

    pub fn at(&self) -> BucketAt {
        BucketAt::new(&self.bucket, &self.prefix)
    }

    /// The name to list it under: what was typed, else the bucket's (and prefix's).
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_string();
        }
        let at = self.at();
        match at.prefix.is_empty() {
            true => at.bucket,
            false => format!("{}/{}", at.bucket, at.prefix),
        }
    }

    /// Where its service is. An endpoint typed without a scheme is `https`.
    pub fn config(&self) -> S3Config {
        let endpoint = self.endpoint.trim().trim_end_matches('/');
        let endpoint = match endpoint {
            "" => None,
            e if e.contains("://") => Some(e.to_string()),
            e => Some(format!("https://{e}")),
        };
        let region = match (self.region.trim(), &endpoint) {
            ("", None) => "us-east-1".to_string(),
            ("", Some(_)) => "auto".to_string(),
            (r, _) => r.to_string(),
        };
        S3Config { endpoint, region }
    }

    /// The key typed, if both halves were.
    pub fn keys(&self) -> Option<Keys> {
        let (id, secret) = (self.access_key_id.trim(), self.secret_access_key.trim());
        (!id.is_empty() && !secret.is_empty()).then(|| Keys {
            access_key_id: id.to_string(),
            secret_access_key: secret.to_string(),
        })
    }

    /// What's wrong with it as typed, if anything. `new`: adding a bucket, which
    /// needs its key; changing one may leave the key as saved.
    pub fn problem(&self, new: bool) -> Option<&'static str> {
        let bucket = self.bucket.trim();
        let half_a_key =
            self.access_key_id.trim().is_empty() != self.secret_access_key.trim().is_empty();
        if bucket.is_empty() {
            Some("Which bucket?")
        } else if bucket.contains('/') || bucket.contains("://") {
            Some("The bucket is just its name; a folder inside it goes in the prefix.")
        } else if half_a_key || (new && self.keys().is_none()) {
            Some("An access key has two parts: its ID and its secret.")
        } else {
            None
        }
    }
}

// ---- access keys ------------------------------------------------------------------------

/// A bucket's access key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Keys {
    #[serde(rename = "id")]
    pub access_key_id: String,
    #[serde(rename = "secret")]
    pub secret_access_key: String,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Keys({}, …)", self.access_key_id)
    }
}

/// Where access keys are kept, by account: [`account`] of a bucket source.
pub trait Secrets: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<Keys>, String>;
    fn set(&self, account: &str, keys: &Keys) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// What a bucket's key is filed under: the bucket (and prefix) at its service. Not the
/// source's id, which a new library (or a copy of this one, `EARSHOT_DATA_DIR`) would
/// give to some other bucket.
pub fn account(at: &BucketAt, config: &S3Config) -> String {
    format!(
        "{} at {}",
        at.url(),
        config.endpoint.as_deref().unwrap_or("aws")
    )
}

/// The OS keychain: the Secret Service on Linux (GNOME Keyring, KWallet), the Keychain
/// on macOS, the Credential Manager on Windows.
pub struct Keychain;

/// The keychain entries' service name.
const KEYCHAIN_SERVICE: &str = "earshot";

impl Keychain {
    fn entry(account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(KEYCHAIN_SERVICE, account).map_err(keychain_error)
    }
}

fn keychain_error(e: keyring::Error) -> String {
    match e {
        keyring::Error::NoDefaultStore | keyring::Error::NoStorageAccess(_) => format!(
            "the system keychain isn't available ({e}): on Linux it's a Secret Service, \
             such as GNOME Keyring or KWallet, which must be running and unlocked"
        ),
        e => format!("the system keychain: {e}"),
    }
}

impl Secrets for Keychain {
    fn get(&self, account: &str) -> Result<Option<Keys>, String> {
        match Keychain::entry(account)?.get_password() {
            Ok(json) => serde_json::from_str(&json)
                .map(Some)
                .map_err(|e| format!("the access key saved for {account} is unreadable: {e}")),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(keychain_error(e)),
        }
    }

    fn set(&self, account: &str, keys: &Keys) -> Result<(), String> {
        let json = serde_json::to_string(keys).expect("keys serialise");
        Keychain::entry(account)?
            .set_password(&json)
            .map_err(keychain_error)
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match Keychain::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(keychain_error(e)),
        }
    }
}

/// Keys kept in memory, for tests (and nothing else: they're gone when it is).
#[cfg(test)]
#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, Keys>>);

#[cfg(test)]
impl Secrets for MemorySecrets {
    fn get(&self, account: &str) -> Result<Option<Keys>, String> {
        Ok(self.0.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, keys: &Keys) -> Result<(), String> {
        self.0.lock().unwrap().insert(account.into(), keys.clone());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        self.0.lock().unwrap().remove(account);
        Ok(())
    }
}

// ---- the S3 client ----------------------------------------------------------------------

/// An object as its bucket lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub key: String,
    pub size: u64,
    pub etag: Option<String>,
    /// When it was last written, ns since the epoch.
    pub modified_ns: i64,
}

/// The threads the S3 client's requests run on. The app itself is threads, not async:
/// each call here blocks its caller until the request is done.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("earshot-s3")
            .enable_all()
            .build()
            .expect("start the S3 client's runtime")
    })
}

/// A connection to one bucket. Reads only: nothing here writes to a bucket.
pub struct Client {
    store: AmazonS3,
}

impl Client {
    pub fn new(at: &BucketAt, config: &S3Config, keys: &Keys) -> Result<Client, String> {
        let options = ClientOptions::new()
            .with_connect_timeout(Duration::from_secs(15))
            // No limit on a whole request (a download takes as long as it takes), but
            // one that stops arriving is given up on.
            .with_timeout_disabled()
            .with_read_timeout(Duration::from_secs(45));
        let mut b = AmazonS3Builder::new()
            .with_bucket_name(&at.bucket)
            .with_region(&config.region)
            .with_access_key_id(&keys.access_key_id)
            .with_secret_access_key(&keys.secret_access_key)
            .with_client_options(options)
            .with_retry(RetryConfig {
                backoff: BackoffConfig::default(),
                max_retries: 3,
                retry_timeout: Duration::from_secs(40),
            });
        if let Some(endpoint) = &config.endpoint {
            // A plain-http endpoint is the user's own choice (a MinIO on this network).
            b = b
                .with_endpoint(endpoint)
                .with_allow_http(endpoint.starts_with("http://"));
        }
        let store = b.build().map_err(|e| explain(&e))?;
        Ok(Client { store })
    }

    fn key(key: &str) -> Result<Key, String> {
        // Parsed, not converted: `Key::from` would percent-encode brackets, hashes and
        // the like, which are part of plenty of file names.
        Key::parse(key).map_err(|e| e.to_string())
    }

    /// Every object under `prefix` (the whole bucket if it's empty).
    pub fn list(&self, prefix: &str) -> Result<Vec<Object>, String> {
        let prefix = match prefix {
            "" => None,
            p => Some(Client::key(p)?),
        };
        runtime().block_on(async {
            let mut objects = vec![];
            let mut stream = self.store.list(prefix.as_ref());
            while let Some(o) = stream.next().await {
                let o = o.map_err(|e| explain(&e))?;
                objects.push(Object {
                    key: o.location.to_string(),
                    size: o.size,
                    etag: o.e_tag,
                    modified_ns: o.last_modified.timestamp_nanos_opt().unwrap_or(0),
                });
            }
            Ok(objects)
        })
    }

    /// Whether `prefix` can be listed with this key: asks for the first page only.
    pub fn check(&self, prefix: &str) -> Result<(), String> {
        let prefix = match prefix {
            "" => None,
            p => Some(Client::key(p)?),
        };
        runtime().block_on(async {
            match self.store.list(prefix.as_ref()).next().await {
                Some(Err(e)) => Err(explain(&e)),
                _ => Ok(()),
            }
        })
    }

    /// Bytes `range` of an object (less, if it ends sooner), and the object's size.
    pub fn range(&self, key: &str, range: Range<u64>) -> Result<(Bytes, u64), String> {
        let key = Client::key(key)?;
        runtime().block_on(async {
            let options = GetOptions {
                range: Some(GetRange::Bounded(range)),
                ..Default::default()
            };
            let got = self
                .store
                .get_opts(&key, options)
                .await
                .map_err(|e| explain(&e))?;
            let size = got.meta.size;
            let bytes = got.bytes().await.map_err(|e| explain(&e))?;
            Ok((bytes, size))
        })
    }

    /// Download a whole object to the file `to`, saying how far it's got (bytes done,
    /// of) as it goes. Returns its size.
    pub fn download(
        &self,
        key: &str,
        to: &Path,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<u64, String> {
        let key = Client::key(key)?;
        let mut file = std::fs::File::create(to).map_err(|e| format!("{}: {e}", to.display()))?;
        runtime().block_on(async {
            let got = self.store.get(&key).await.map_err(|e| explain(&e))?;
            let size = got.meta.size;
            let mut stream = got.into_stream();
            let mut done = 0;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| explain(&e))?;
                file.write_all(&chunk)
                    .map_err(|e| format!("{}: {e}", to.display()))?;
                done += chunk.len() as u64;
                progress(done, size);
            }
            if done != size {
                return Err(format!("the download stopped at {done} of {size} bytes"));
            }
            file.flush().map_err(|e| format!("{}: {e}", to.display()))?;
            Ok(size)
        })
    }
}

/// Whether a request failed because the service couldn't be reached at all (no
/// network, no such host, nothing listening, no reply), rather than it answering
/// with an error.
fn unreachable(e: &object_store::Error) -> bool {
    use object_store::client::{HttpError, HttpErrorKind};
    // Going by what kind of HTTP failure is at the bottom of the error, not by its
    // words: a service's own error may well say "timeout".
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(e);
    while let Some(c) = cause {
        if let Some(http) = c.downcast_ref::<HttpError>() {
            return matches!(
                http.kind(),
                HttpErrorKind::Connect
                    | HttpErrorKind::Request
                    | HttpErrorKind::Timeout
                    | HttpErrorKind::Interrupted
            );
        }
        cause = c.source();
    }
    false
}

/// An S3 client error in a line: object_store's own run to several sentences, with the
/// useful part (the service's answer) at the end.
fn explain(e: &object_store::Error) -> String {
    let text = e.to_string();
    let has = |needle: &str| text.contains(needle);
    if has("InvalidAccessKeyId") || has("SignatureDoesNotMatch") || has("InvalidToken") {
        "the service doesn't accept that access key".into()
    } else if has("AccessDenied") || has("403 Forbidden") {
        "access denied: the key needs to be allowed to list and read this bucket (and prefix)"
            .into()
    } else if has("NoSuchBucket") {
        "there's no bucket of that name there".into()
    } else if matches!(e, object_store::Error::NotFound { .. }) {
        "it isn't there (any more)".into()
    } else if unreachable(e) {
        // (The client's own account of it, with its retries, is for the log.)
        eprintln!("s3: {}", last_line(&text));
        "couldn't reach the service: there's no connection, or it isn't answering".into()
    } else {
        last_line(&text)
    }
}

/// The last sentence of a chain of "…: …: …" errors, kept short.
fn last_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or(text).trim();
    let mut out: String = line.chars().take(300).collect();
    if out.len() < line.len() {
        out.push('…');
    }
    out
}

// ---- reading an object a range at a time ------------------------------------------------

/// The first range read of an object is this long; reading on doubles it, up to
/// [`RANGE_MAX`]. Most tags fit the first; embedded cover art takes a few more.
const RANGE_FIRST: u64 = 64 * 1024;
const RANGE_MAX: u64 = 1024 * 1024;

/// An object read where it is, by ranged GETs: for its tags and cover art, which are a
/// few KB at its start (and, in some formats, its end). What's been read is kept, so
/// going back costs nothing. [`Ranged::another`] reads the same object from the start
/// again, sharing what's been fetched.
pub struct Ranged {
    shared: Arc<RangedShared>,
    pos: u64,
}

struct RangedShared {
    client: Arc<Client>,
    key: String,
    /// The object's size, once known (from the listing, or the first range's reply).
    size: OnceLock<u64>,
    state: Mutex<RangedState>,
    /// A read failed on the way (the network, not the file): what was read of it
    /// can't be trusted to be all there is.
    failed: AtomicBool,
}

struct RangedState {
    /// What's been fetched: (offset, bytes), not overlapping.
    chunks: Vec<(u64, Bytes)>,
    /// How much to ask for next.
    ahead: u64,
    /// Where the last range fetched ended: reading on from there is reading on.
    last_end: u64,
}

impl Ranged {
    /// `size`: the object's, if it's known (else the first read finds out).
    pub fn new(client: Arc<Client>, key: &str, size: Option<u64>) -> Ranged {
        let known = OnceLock::new();
        if let Some(s) = size {
            let _ = known.set(s);
        }
        Ranged {
            shared: Arc::new(RangedShared {
                client,
                key: key.to_string(),
                size: known,
                state: Mutex::new(RangedState {
                    chunks: vec![],
                    ahead: RANGE_FIRST,
                    last_end: 0,
                }),
                failed: AtomicBool::new(false),
            }),
            pos: 0,
        }
    }

    /// The same object from its start, sharing what's been fetched.
    pub fn another(&self) -> Ranged {
        Ranged {
            shared: self.shared.clone(),
            pos: 0,
        }
    }

    /// Whether a read of it failed on the way: the network's doing, not the file's.
    pub fn failed(&self) -> bool {
        self.shared.failed.load(Ordering::Relaxed)
    }

    /// The bytes at `pos`, fetching a range there if they haven't been.
    fn at(&self, pos: u64) -> io::Result<Bytes> {
        let s = &self.shared;
        let mut state = s.state.lock().unwrap();
        if let Some((start, bytes)) = state
            .chunks
            .iter()
            .find(|(start, b)| (*start..*start + b.len() as u64).contains(&pos))
        {
            return Ok(bytes.slice((pos - start) as usize..));
        }
        // Reading on gets more each time; a jump elsewhere starts small again.
        if pos != state.last_end {
            state.ahead = RANGE_FIRST;
        }
        let mut end = pos + state.ahead;
        // Up to what's been fetched already, so chunks don't overlap.
        if let Some(next) = state
            .chunks
            .iter()
            .map(|c| c.0)
            .filter(|&start| start > pos)
            .min()
        {
            end = end.min(next);
        }
        if let Some(&size) = s.size.get() {
            end = end.min(size);
        }
        let (bytes, size) = s.client.range(&s.key, pos..end).map_err(|e| {
            s.failed.store(true, Ordering::Relaxed);
            io::Error::other(format!("{}: {e}", s.key))
        })?;
        let _ = s.size.set(size);
        state.last_end = pos + bytes.len() as u64;
        state.ahead = (state.ahead * 2).min(RANGE_MAX);
        state.chunks.push((pos, bytes.clone()));
        Ok(bytes)
    }

    fn size(&self) -> io::Result<u64> {
        if let Some(&s) = self.shared.size.get() {
            return Ok(s);
        }
        // The first range's reply says.
        self.at(0)?;
        Ok(self.shared.size.get().copied().unwrap_or(0))
    }
}

impl Read for Ranged {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.size()? {
            return Ok(0);
        }
        let bytes = self.at(self.pos)?;
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Ranged {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => self.size()?.checked_add_signed(d),
        };
        self.pos = pos.ok_or_else(|| io::Error::other("seek before the start"))?;
        Ok(self.pos)
    }
}

impl Media for Ranged {
    fn len(&self) -> Option<u64> {
        self.size().ok()
    }
}

// ---- the cache of downloaded tracks -----------------------------------------------------

/// Downloaded tracks: `<cache folder>/s3/<bucket>/<hash of the URL>.<ext>`, found by
/// the object's URL alone (nothing about them is in the database, so a cleaner that
/// empties the cache folder loses nothing but the downloads). A copy's modified time
/// is when it was last used.
pub struct Cache {
    root: PathBuf,
    /// The most it may take, bytes.
    limit: AtomicU64,
    /// What it takes now, bytes, as of the last [`Cache::tidy`] plus downloads since.
    usage: AtomicU64,
    /// URLs whose copies are never removed to make room.
    pinned: RwLock<HashSet<String>>,
}

/// A download that isn't finished: `<name>.part`, renamed when it's all there.
const PART: &str = "part";

impl Cache {
    pub fn new(cache_folder: &Path, limit: u64) -> Cache {
        Cache {
            root: cache_folder.join("s3"),
            limit: AtomicU64::new(limit),
            usage: AtomicU64::new(0),
            pinned: RwLock::default(),
        }
    }

    /// Where an object's copy is, or would be.
    pub fn path(&self, url: &str) -> PathBuf {
        let (bucket, _) = split_url(url).unwrap_or(("other", ""));
        let digest: [u8; 32] = Sha256::digest(url.as_bytes()).into();
        let mut name = hash::to_hex(&digest)[..32].to_string();
        // Its extension says what kind of audio it is.
        if let Some(ext) = Location::Url(url.into()).extension()
            && ext.chars().all(|c| c.is_ascii_alphanumeric())
        {
            name = format!("{name}.{ext}");
        }
        self.root.join(safe_name(bucket)).join(name)
    }

    /// The object's copy, if it's here.
    pub fn cached(&self, url: &str) -> Option<PathBuf> {
        let p = self.path(url);
        p.is_file().then_some(p)
    }

    /// The copy was used just now: it's the last to go when room is needed.
    fn touch(path: &Path) {
        if let Ok(f) = std::fs::File::options().write(true).open(path) {
            let _ = f.set_modified(SystemTime::now());
        }
    }

    /// Remove an object's copy (the object changed, or its source went).
    pub fn forget(&self, url: &str) {
        let p = self.path(url);
        if let Ok(m) = std::fs::metadata(&p)
            && std::fs::remove_file(&p).is_ok()
        {
            self.sub_usage(m.len());
        }
    }

    /// Remove every copy of a bucket's objects (its source was removed).
    pub fn forget_bucket(&self, bucket: &str) {
        let _ = std::fs::remove_dir_all(self.root.join(safe_name(bucket)));
        self.tidy(None);
    }

    fn sub_usage(&self, n: u64) {
        let _ = self
            .usage
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                Some(u.saturating_sub(n))
            });
    }

    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Change the most it may take, and make it fit.
    pub fn set_limit(&self, limit: u64) {
        self.limit.store(limit, Ordering::Relaxed);
        self.tidy(None);
    }

    /// What it takes on disk, bytes.
    pub fn usage(&self) -> u64 {
        self.usage.load(Ordering::Relaxed)
    }

    /// These objects' copies (and no others) are never removed to make room.
    pub fn set_pinned(&self, urls: HashSet<String>) {
        *self.pinned.write().unwrap() = urls;
    }

    /// Remove downloads a crash left half done (at startup, when none is under way).
    pub fn clear_parts(&self) {
        for (path, _, _) in self.files() {
            if path.extension().is_some_and(|e| e == PART) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// The path of every download in the cache: an object is downloaded if its
    /// [`Cache::path`] is among them. (One look at the folder, for asking about a
    /// whole library's tracks at once.)
    pub fn present(&self) -> HashSet<PathBuf> {
        self.files()
            .into_iter()
            .map(|f| f.0)
            .filter(|p| p.extension().is_none_or(|e| e != PART))
            .collect()
    }

    /// Every file in the cache: (path, size, last used).
    fn files(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let mut out = vec![];
        for bucket in std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
        {
            for f in std::fs::read_dir(bucket.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                if let Ok(m) = f.metadata()
                    && m.is_file()
                {
                    let used = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    out.push((f.path(), m.len(), used));
                }
            }
        }
        out
    }

    /// Make it fit its limit: the copies used least recently go first. Never the
    /// pinned ones, or `keep` (the one just downloaded, about to be read), so it can
    /// stay over the limit when those alone are. Returns what it takes afterwards.
    pub fn tidy(&self, keep: Option<&Path>) -> u64 {
        let limit = self.limit();
        let pinned: HashSet<PathBuf> = self
            .pinned
            .read()
            .unwrap()
            .iter()
            .map(|u| self.path(u))
            .collect();
        let mut files = self.files();
        // Oldest first.
        files.sort_by_key(|f| f.2);
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        for (path, size, _) in &files {
            if total <= limit {
                break;
            }
            if Some(path.as_path()) == keep
                || pinned.contains(path)
                || path.extension().is_some_and(|e| e == PART)
            {
                continue;
            }
            if std::fs::remove_file(path).is_ok() {
                total -= size;
            }
        }
        self.usage.store(total, Ordering::Relaxed);
        total
    }
}

/// A bucket's name as a folder's: bucket names are already tame (lower case, digits,
/// dots, hyphens), but what's in the database is only what someone typed.
fn safe_name(bucket: &str) -> String {
    let name: String = bucket
        .chars()
        .map(|c| match c {
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => c,
            '.' => '.',
            _ => '_',
        })
        .collect();
    match name.trim_matches('.') {
        "" => "bucket".into(),
        n => n.to_string(),
    }
}

// ---- the buckets the app has ------------------------------------------------------------

/// What the UI asks of bucket sources. Run by the UI (it has the sources and the
/// library), like `LibraryCmd`.
#[derive(Debug, Clone)]
pub enum BucketCmd {
    /// Check the bucket filled in can be listed with its key, then save it: a new
    /// source, or `source` changed. Answered with [`BucketEvent::Saved`].
    Save {
        form: BucketForm,
        source: Option<SourceId>,
    },
    /// List a bucket again and read what's new or changed.
    Scan(SourceId),
    /// Download these tracks (those that are a bucket's) to the cache now.
    Download(Vec<TrackId>),
    /// Remove these tracks' downloads from the cache.
    RemoveDownloads(Vec<TrackId>),
    /// Keep a crate's, playlist's or folder's bucket tracks downloaded, whatever room
    /// the cache needs (and download the ones that aren't), or stop keeping them.
    KeepOffline { collection: CollectionId, on: bool },
    /// The most the cache of downloads may take, GB (and save it).
    CacheGb(f64),
}

/// What happened to a bucket, for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum BucketEvent {
    /// The bucket the user filled in was checked and added (or changed), or couldn't be.
    Saved(Result<SourceId, String>),
    /// A bucket was read through: how many tracks it has, and how many of those
    /// couldn't be read this time.
    Scanned {
        source: SourceId,
        result: Result<Scan, String>,
    },
    /// An object was downloaded to the cache.
    Downloaded { url: String },
    /// Tracks asked to be downloaded ([`Buckets::download`]) have been, or couldn't be.
    Fetched { done: usize, failed: Vec<String> },
    /// Downloads were removed from the cache (its limit changed).
    CacheChanged,
}

/// What a scan of a bucket found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Scan {
    /// Audio files in the bucket.
    pub tracks: usize,
    /// New or changed since the last scan, and read.
    pub read: usize,
    /// Couldn't be read (the network): left for the next scan.
    pub failed: usize,
}

/// Where [`BucketEvent`]s go.
pub type Tell = Arc<dyn Fn(BucketEvent) + Send + Sync>;

/// One bucket source, as the `s3://` remote needs it.
struct Entry {
    /// Which app's (tests run several in one process).
    owner: usize,
    source: SourceId,
    name: String,
    at: BucketAt,
    config: S3Config,
    secrets: Arc<dyn Secrets>,
    cache: Arc<Cache>,
    jobs: Jobs,
    tell: Tell,
    /// Made when first needed (it reads the keychain), then kept.
    client: Mutex<Option<Arc<Client>>>,
}

impl Entry {
    fn client(&self) -> Result<Arc<Client>, String> {
        let mut client = self.client.lock().unwrap();
        if let Some(c) = &*client {
            return Ok(c.clone());
        }
        let keys = self
            .secrets
            .get(&account(&self.at, &self.config))?
            .ok_or_else(|| {
                format!(
                    "there's no access key saved for {}: give it again (Sources > \
                     right-click the bucket > Edit…)",
                    self.name
                )
            })?;
        let c = Arc::new(Client::new(&self.at, &self.config, &keys)?);
        *client = Some(c.clone());
        Ok(c)
    }
}

/// Every app's bucket sources (there's one app, except in tests), which the `s3://`
/// remote looks URLs up in.
fn entries() -> &'static RwLock<Vec<Arc<Entry>>> {
    static ENTRIES: OnceLock<RwLock<Vec<Arc<Entry>>>> = OnceLock::new();
    ENTRIES.get_or_init(Default::default)
}

/// The source an object's URL belongs to: the one for its bucket whose prefix holds
/// it (the longest, if prefixes nest).
fn entry_for(url: &str) -> Result<(Arc<Entry>, &str), String> {
    let (bucket, key) = split_url(url).ok_or_else(|| format!("{url}: not a bucket object"))?;
    entries()
        .read()
        .unwrap()
        .iter()
        .filter(|e| e.at.bucket == bucket && e.at.holds(key))
        .max_by_key(|e| e.at.prefix.len())
        .cloned()
        .map(|e| (e, key))
        .ok_or_else(|| format!("{url}: its bucket isn't one of the sources (any more)"))
}

/// Downloads under way, by URL: a second request for one waits for the first.
fn in_flight() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    static IN_FLIGHT: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(Default::default)
}

/// The `s3://` remote (`earshot_audio::media`): how everything that reads audio reads
/// a bucket's.
struct S3;

impl Remote for S3 {
    fn cached(&self, url: &str) -> Option<PathBuf> {
        entry_for(url).ok()?.0.cache.cached(url)
    }

    fn fetch(&self, url: &str) -> Result<PathBuf, String> {
        let (e, key) = entry_for(url)?;
        let path = e.cache.path(url);
        if path.is_file() {
            Cache::touch(&path);
            return Ok(path);
        }
        // One download of it at a time: whoever else wants it waits here, then
        // finds it.
        let gate = in_flight()
            .lock()
            .unwrap()
            .entry(url.to_string())
            .or_default()
            .clone();
        let result = {
            let _turn = gate.lock().unwrap();
            if path.is_file() {
                Ok(path)
            } else {
                download(&e, key, url, &path).map(|()| path)
            }
        };
        in_flight().lock().unwrap().remove(url);
        result
    }

    fn peek(&self, url: &str) -> Result<Box<dyn Media>, String> {
        let (e, key) = entry_for(url)?;
        Ok(Box::new(Ranged::new(e.client()?, key, None)))
    }
}

/// Download an object into the cache at `path`, then make room if the cache is over
/// its limit.
fn download(e: &Entry, key: &str, url: &str, path: &Path) -> Result<(), String> {
    let name = Location::Url(url.into()).file_name();
    let job = e.jobs.start(format!("Downloading {name} from {}", e.name));
    let client = e.client().map_err(|err| format!("{name}: {err}"))?;
    let dir = path.parent().expect("a cache path has a folder");
    std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
    let mut part = path.as_os_str().to_owned();
    part.push(".");
    part.push(PART);
    let part = PathBuf::from(part);
    // In MB, as the status line counts.
    let mb = |b: u64| b.div_ceil(1_000_000);
    let got = client.download(key, &part, |done, of| job.progress(mb(done), mb(of)));
    match got.and_then(|size| {
        std::fs::rename(&part, path)
            .map(|()| size)
            .map_err(|err| format!("{}: {err}", path.display()))
    }) {
        Ok(size) => {
            e.cache.usage.fetch_add(size, Ordering::Relaxed);
            if e.cache.usage() > e.cache.limit() {
                e.cache.tidy(Some(path));
            }
            (e.tell)(BucketEvent::Downloaded { url: url.into() });
            Ok(())
        }
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            Err(format!("downloading {name} from {}: {err}", e.name))
        }
    }
}

/// The app's bucket sources: what the UI and the scans talk to. Making one registers
/// the `s3://` remote.
#[derive(Clone)]
pub struct Buckets {
    owner: usize,
    pub cache: Arc<Cache>,
    pub secrets: Arc<dyn Secrets>,
    jobs: Jobs,
    tell: Tell,
}

impl Buckets {
    /// `cache_folder`: the app's (`Folders::cache`); `limit`: the most downloads may
    /// take there, bytes.
    pub fn new(
        cache_folder: &Path,
        limit: u64,
        secrets: Arc<dyn Secrets>,
        jobs: Jobs,
        tell: Tell,
    ) -> Buckets {
        static OWNERS: AtomicU64 = AtomicU64::new(1);
        static REGISTER: std::sync::Once = std::sync::Once::new();
        REGISTER.call_once(|| media::register(SCHEME, Arc::new(S3)));
        let cache = Arc::new(Cache::new(cache_folder, limit));
        Buckets {
            owner: OWNERS.fetch_add(1, Ordering::Relaxed) as usize,
            cache,
            secrets,
            jobs,
            tell,
        }
    }

    /// At startup, in the background: clear out half-done downloads and make the
    /// cache fit its limit.
    pub fn tidy_at_start(&self) {
        let cache = self.cache.clone();
        super::background::spawn("earshot-s3-cache", move || {
            cache.clear_parts();
            cache.tidy(None);
        });
    }

    /// The sources, whenever they change (`StoreEvent::Sources`): the enabled buckets
    /// among them are the ones whose URLs can be read from now on.
    pub fn set_sources(&self, sources: &[Source]) {
        let mut all = entries().write().unwrap();
        let (mine, mut others): (Vec<_>, Vec<_>) =
            all.drain(..).partition(|e| e.owner == self.owner);
        for s in sources.iter().filter(|s| s.enabled) {
            let Some(at) = s.bucket_url().and_then(|u| BucketAt::parse(&u)) else {
                continue;
            };
            let config = s.config.s3.clone().unwrap_or_default();
            // As it was (with its client), if nothing about it changed.
            let same = mine
                .iter()
                .find(|e| e.source == s.id && e.at == at && e.config == config);
            others.push(match same {
                Some(e) if e.name == s.name => e.clone(),
                _ => Arc::new(Entry {
                    owner: self.owner,
                    source: s.id,
                    name: s.name.clone(),
                    at,
                    config,
                    secrets: self.secrets.clone(),
                    cache: self.cache.clone(),
                    jobs: self.jobs.clone(),
                    tell: self.tell.clone(),
                    client: Mutex::new(same.and_then(|e| e.client.lock().unwrap().clone())),
                }),
            });
        }
        *all = others;
    }

    /// A source's key changed: its next request connects afresh.
    pub fn forget_client(&self, source: SourceId) {
        for e in entries().read().unwrap().iter() {
            if e.owner == self.owner && e.source == source {
                *e.client.lock().unwrap() = None;
            }
        }
    }

    fn entry(&self, source: SourceId) -> Result<Arc<Entry>, String> {
        entries()
            .read()
            .unwrap()
            .iter()
            .find(|e| e.owner == self.owner && e.source == source)
            .cloned()
            .ok_or_else(|| "that bucket isn't one of the sources (any more)".to_string())
    }

    /// These tracks' copies (by URL), and no others, stay in the cache whatever room
    /// is needed (the tracks of crates and playlists kept offline).
    pub fn pin(&self, urls: HashSet<String>) {
        self.cache.set_pinned(urls);
    }

    /// Download these objects to the cache, one after another, in the background:
    /// "make available offline". Answered with [`BucketEvent::Fetched`].
    pub fn download(&self, urls: Vec<String>) {
        let (jobs, tell) = (self.jobs.clone(), self.tell.clone());
        super::background::spawn("earshot-s3-download", move || {
            let job = jobs.start("Making tracks available offline");
            let stop = Arc::new(AtomicBool::new(false));
            let asked = stop.clone();
            job.on_cancel(move || asked.store(true, Ordering::Relaxed));
            let (mut done, mut failed) = (0, vec![]);
            for (i, url) in urls.iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                job.progress(i as u64, urls.len() as u64);
                match Location::Url(url.clone()).local() {
                    Ok(_) => done += 1,
                    Err(e) => failed.push(e),
                }
            }
            drop(job);
            tell(BucketEvent::Fetched { done, failed });
        });
    }
}

// ---- adding and scanning ----------------------------------------------------------------

/// Check the bucket the user filled in can be listed with its key, then save it: the
/// key in the keychain, the source in the Store (which lists it, `StoreEvent::Sources`).
/// `existing`: the source being changed, if it's not a new one. Returns its id.
/// Blocking: call from a background thread.
pub fn save(
    store: &Store,
    buckets: &Buckets,
    form: &BucketForm,
    existing: Option<&Source>,
) -> Result<SourceId, String> {
    if let Some(p) = form.problem(existing.is_none()) {
        return Err(p.into());
    }
    let (at, config) = (form.at(), form.config());
    let was = existing.and_then(|s| {
        let at = BucketAt::parse(&s.bucket_url()?)?;
        Some(account(&at, &s.config.s3.clone().unwrap_or_default()))
    });
    let keys = match (form.keys(), &was) {
        (Some(k), _) => k,
        // Unchanged: the one saved.
        (None, Some(was)) => buckets
            .secrets
            .get(was)?
            .ok_or("There's no access key saved for this bucket: give it again.")?,
        (None, None) => return Err("An access key has two parts: its ID and its secret.".into()),
    };
    Client::new(&at, &config, &keys)?
        .check(&at.prefix)
        .map_err(|e| format!("Couldn't list {}: {e}", at.url()))?;
    let acct = account(&at, &config);
    buckets.secrets.set(&acct, &keys)?;
    let new = super::sources::NewSource {
        kind: super::sources::SourceKind::S3,
        name: form.display_name(),
        location: Some(PathBuf::from(at.url())),
        config: super::sources::SourceConfig {
            s3: Some(config),
            ..Default::default()
        },
    };
    let id = match existing {
        Some(s) => {
            let changed = Source {
                name: new.name,
                config: new.config,
                ..s.clone()
            };
            store
                .call(move |w| w.update_source(&changed))
                .map_err(|e| e.to_string())?;
            s.id
        }
        None => store
            .call(move |w| w.add_source(&new))
            .map_err(|e| e.to_string())?,
    };
    // Its service moved: the key is filed under the new one only.
    if let Some(was) = was.filter(|w| *w != acct) {
        let _ = buckets.secrets.delete(&was);
    }
    buckets.forget_client(id);
    Ok(id)
}

/// A bucket source was removed: its key and its downloads go too.
pub fn removed(buckets: &Buckets, source: &Source, others: &[Source]) {
    let Some(at) = source.bucket_url().and_then(|u| BucketAt::parse(&u)) else {
        return;
    };
    let config = source.config.s3.clone().unwrap_or_default();
    if let Err(e) = buckets.secrets.delete(&account(&at, &config)) {
        eprintln!("removing {}'s access key: {e}", source.name);
    }
    // Unless another source reads the same bucket (another prefix of it).
    let shared = others.iter().any(|s| {
        s.id != source.id
            && s.bucket_url()
                .and_then(|u| BucketAt::parse(&u))
                .is_some_and(|a| a.bucket == at.bucket)
    });
    if !shared {
        buckets.cache.forget_bucket(&at.bucket);
    }
}

/// Tags are read from this many objects at once: it's nearly all waiting on the
/// network (a few small ranged GETs each), and S3-compatible services take far more
/// requests at once than this.
const SCAN_THREADS: usize = 32;
/// Objects are recorded this many at a time, so a long first scan shows as it goes
/// (and what it's read survives being cut short).
const SCAN_BATCH: usize = 200;

/// List a bucket source's audio files and record them in the Store, which then sends
/// the library (`StoreEvent::Library`). Only objects that are new, or whose ETag or
/// size changed, have their tags read: a few ranged GETs each, nothing downloaded.
/// Objects that have gone from the bucket are marked missing. Blocking: call from a
/// background thread.
pub fn scan(store: &Store, buckets: &Buckets, source: SourceId) -> Result<Scan, String> {
    let e = buckets.entry(source)?;
    let job = e.jobs.start(format!("Reading {}", e.name));
    job.say("Listing the bucket");
    let client = e.client()?;
    let root = e.at.url();
    let mut objects: Vec<Object> = client
        .list(&e.at.prefix)?
        .into_iter()
        .filter(|o| is_track(&o.key))
        .collect();
    objects.sort_by(|a, b| a.key.cmp(&b.key));
    let known = store.reader.objects(&root).map_err(|err| err.to_string())?;
    let changed: Vec<&Object> = objects
        .iter()
        .filter(|o| known.get(&e.at.object_url(&o.key)) != Some(&(o.size as i64, o.etag.clone())))
        .collect();

    job.say("Reading tags");
    let total = changed.len() as u64;
    let done = AtomicU64::new(0);
    let mut failed = 0;
    for batch in changed.chunks(SCAN_BATCH) {
        let per = batch.len().div_ceil(SCAN_THREADS).max(1);
        let rows: Vec<Option<ScannedObject>> = std::thread::scope(|s| {
            let handles: Vec<_> = batch
                .chunks(per)
                .map(|some| {
                    let (client, e, job, done) = (&client, &e, &job, &done);
                    s.spawn(move || {
                        some.iter()
                            .map(|o| {
                                let row = read_object(client, &e.at, o);
                                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                                job.progress(n, total);
                                row
                            })
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });
        failed += rows.iter().filter(|r| r.is_none()).count();
        let rows: Vec<ScannedObject> = rows.into_iter().flatten().collect();
        // A changed object's old copy isn't it any more.
        for r in &rows {
            if known.contains_key(&r.url) {
                e.cache.forget(&r.url);
            }
        }
        store
            .call(move |w| w.record_objects(source, &rows))
            .map_err(|err| err.to_string())?;
    }

    let found: Vec<(String, String)> = objects
        .iter()
        .map(|o| (e.at.object_url(&o.key), o.key.clone()))
        .collect();
    let tracks = found.len();
    store
        .call(move |w| w.bucket_listed(source, &root, &found))
        .map_err(|err| err.to_string())?;
    Ok(Scan {
        tracks,
        read: changed.len() - failed,
        failed,
    })
}

/// An object earshot lists: audio by its extension, and not hidden (no part of its
/// key starts with a dot).
fn is_track(key: &str) -> bool {
    !key.split('/').any(|part| part.starts_with('.'))
        && Location::Url(format!("s3://b/{key}"))
            .extension()
            .is_some_and(|ext| library::is_audio_extension(&ext))
}

/// Read an object's tags (and its stamped audio hash) where it is. `None` if the
/// network failed on the way: it's left for the next scan, rather than recorded
/// without the tags it has. A file that just can't be parsed is recorded untagged, as
/// one in a folder is.
fn read_object(client: &Arc<Client>, at: &BucketAt, o: &Object) -> Option<ScannedObject> {
    let url = at.object_url(&o.key);
    let loc = Location::Url(url.clone());
    let ext = loc.extension();
    let media = Ranged::new(client.clone(), &o.key, Some(o.size));
    let mut again = media.another();
    let tags: TrackTags =
        decode::read_tags_from(Box::new(media.another()), ext.as_deref()).unwrap_or_default();
    let tag_hash = match ext.as_deref() {
        Some("mp3" | "aac") => hash::tag_hash_from(&mut again),
        _ => None,
    };
    if media.failed() {
        eprintln!("s3: {url}: couldn't be read; left for the next scan");
        return None;
    }
    Some(ScannedObject {
        url,
        key: o.key.clone(),
        size: o.size as i64,
        modified_ns: o.modified_ns,
        etag: o.etag.clone(),
        tags,
        tag_hash,
    })
}

#[cfg(test)]
pub(crate) mod tests;
