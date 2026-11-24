//! Bucket sources (`core::bucket`): adding and changing them, reading them through,
//! downloading their tracks, and keeping collections' tracks downloaded.

use std::collections::HashSet;

use earshot_engine::media::Location;

use super::*;
use crate::core::bucket::{self, BucketCmd, BucketEvent, SCHEME};
use crate::core::notice::Level;
use crate::core::sources::SourceId;

/// Whether a track's path is a bucket object's URL.
fn in_bucket(path: &str) -> bool {
    path.strip_prefix(SCHEME)
        .is_some_and(|rest| rest.starts_with("://"))
}

impl EarshotApp {
    pub(super) fn bucket_cmd(&mut self, c: BucketCmd) {
        match c {
            BucketCmd::Save { form, source } => {
                let existing =
                    source.and_then(|id| self.sources.iter().find(|s| s.id == id).cloned());
                let (store, buckets, sink) = (
                    self.lib.store.clone(),
                    self.buckets.clone(),
                    self.sink.clone(),
                );
                crate::core::background::spawn("earshot-s3", move || {
                    let saved = bucket::save(&store, &buckets, &form, existing.as_ref());
                    sink(Event::Bucket(BucketEvent::Saved(saved)));
                });
            }
            BucketCmd::Scan(source) => self.scan_bucket(source, false),
            BucketCmd::Download(tracks) => {
                let urls = self.bucket_urls(&tracks);
                if !urls.is_empty() {
                    self.buckets.download(urls);
                }
            }
            BucketCmd::RemoveDownloads(tracks) => {
                for url in self.bucket_urls(&tracks) {
                    self.buckets.cache.forget(&url);
                }
            }
            BucketCmd::KeepOffline { collection, on } => {
                if on {
                    self.offline.insert(collection);
                } else {
                    self.offline.remove(&collection);
                }
                let mut ids: Vec<_> = self.offline.iter().copied().collect();
                ids.sort_unstable();
                self.lib
                    .store
                    .send(settings::save(settings::S3_OFFLINE, &ids));
                self.settings.s3_offline = ids;
                // Asked again: what failed before is tried once more.
                self.offline_asked.clear();
                self.sync_offline();
            }
            BucketCmd::CacheGb(gb) => {
                self.settings.s3_cache_gb = gb;
                self.lib
                    .store
                    .send(settings::save(settings::S3_CACHE_GB, &gb));
                // Making it fit removes files: not on this thread.
                let cache = self.buckets.cache.clone();
                crate::core::background::spawn("earshot-s3-cache", move || {
                    cache.set_limit((gb.max(0.0) * 1e9) as u64);
                });
            }
        }
    }

    /// The URLs of the tracks among these that are a bucket's.
    fn bucket_urls(&self, tracks: &[i64]) -> Vec<String> {
        tracks
            .iter()
            .filter_map(|&id| self.index.get(id))
            .filter(|t| in_bucket(&t.path))
            .map(|t| t.path.clone())
            .collect()
    }

    pub(super) fn bucket_event(&mut self, e: BucketEvent) {
        match e {
            BucketEvent::Saved(saved) => {
                self.panels.library.buckets.saved(&saved);
                // (Its `StoreEvent::Sources` came first: the bucket is known by now.)
                if let Ok(source) = saved {
                    self.scan_bucket(source, false);
                }
            }
            BucketEvent::Scanned { source, result } => {
                self.panels.library.buckets.scanned(source, &result);
                let quiet = self.bucket_quiet.remove(&source);
                let name = self
                    .sources
                    .iter()
                    .find(|s| s.id == source)
                    .map_or("the bucket".to_string(), |s| s.name.clone());
                match result {
                    Ok(_) => {}
                    Err(e) if quiet => eprintln!("s3: reading {name}: {e}"),
                    Err(e) => self.notify(Level::Warning, format!("Couldn't read {name}: {e}")),
                }
            }
            // Its audio can be hashed now (and matched with the same audio elsewhere).
            BucketEvent::Downloaded { .. } => self.hasher.send(()),
            BucketEvent::Fetched { done, failed } => match failed.len() {
                0 => {}
                n => {
                    let text = format!(
                        "{n} of {} tracks couldn't be downloaded: {}",
                        done + n,
                        failed[0]
                    );
                    self.notify(Level::Warning, text);
                }
            },
        }
    }

    /// List a bucket again and read what's new or changed, in the background.
    /// `quiet`: not asked for by the user, so a failure is only logged.
    pub(super) fn scan_bucket(&mut self, source: SourceId, quiet: bool) {
        if quiet {
            self.bucket_quiet.insert(source);
        } else {
            self.bucket_quiet.remove(&source);
        }
        self.panels.library.buckets.scanning(source);
        let (store, buckets, sink) = (
            self.lib.store.clone(),
            self.buckets.clone(),
            self.sink.clone(),
        );
        crate::core::background::spawn("earshot-s3-scan", move || {
            let result = bucket::scan(&store, &buckets, source);
            sink(Event::Bucket(BucketEvent::Scanned { source, result }));
        });
    }

    /// Read every bucket through again.
    pub(super) fn scan_buckets(&mut self, quiet: bool) {
        let buckets: Vec<SourceId> = self
            .sources
            .iter()
            .filter(|s| s.kind.is_bucket() && s.enabled)
            .map(|s| s.id)
            .collect();
        for source in buckets {
            self.scan_bucket(source, quiet);
        }
    }

    /// A source was removed: if it was a bucket, its key and downloads go too.
    pub(super) fn bucket_removed(&mut self, source: SourceId) {
        let Some(s) = self
            .sources
            .iter()
            .find(|s| s.id == source && s.kind.is_bucket())
            .cloned()
        else {
            return;
        };
        let (buckets, others) = (self.buckets.clone(), self.sources.clone());
        // The keychain can take a moment (or ask to be unlocked).
        crate::core::background::spawn("earshot-s3", move || {
            bucket::removed(&buckets, &s, &others);
        });
    }

    /// The collections kept offline, their tracks or the library changed: say which
    /// downloads are never to be removed for room, and download the ones that aren't
    /// here yet.
    pub(super) fn sync_offline(&mut self) {
        if self.offline.is_empty() {
            return self.buckets.pin(HashSet::new());
        }
        let mut pinned = HashSet::new();
        for &id in &self.offline {
            let Some(c) = self.collections.get(id) else {
                continue;
            };
            let tracks = match c.kind {
                CollectionKind::Folder => self.collections.tracks_inside(id),
                _ => c.tracks.clone(),
            };
            pinned.extend(self.bucket_urls(&tracks));
        }
        let wanted: Vec<String> = pinned
            .iter()
            .filter(|url| !self.offline_asked.contains(*url))
            .filter(|url| Location::Url((*url).clone()).here().is_none())
            .cloned()
            .collect();
        self.buckets.pin(pinned);
        if !wanted.is_empty() {
            self.offline_asked.extend(wanted.iter().cloned());
            self.buckets.download(wanted);
        }
    }
}
