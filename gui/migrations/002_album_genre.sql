-- Album and genre are now read from files' tags (`TrackTags::album`, `::genre`).
-- Files and objects already scanned would only be read again once they changed, so
-- forget what they were like: the next scan of each folder or bucket reads every
-- one's tags again, once. Only tag-sourced metadata is rewritten; edits made in
-- earshot outrank it and are kept.
UPDATE files SET mtime_ns = 0;
UPDATE files SET etag = NULL WHERE etag IS NOT NULL;
