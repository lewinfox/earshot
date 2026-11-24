-- S3-compatible buckets as sources (core/bucket.rs): AWS S3, Cloudflare R2, Backblaze
-- B2, Wasabi, MinIO. There can be any number, each with its own name.
--
-- A bucket source: `kind` 's3'; `location` is `s3://<bucket>/<prefix>`; `config`
-- holds its endpoint and region (`SourceConfig::s3`). Its access key is never here:
-- it's in the OS keychain, under the source's id.
--
-- Each object is a `files` row whose `path` is its URL (`s3://<bucket>/<key>`), with
-- its size, when it was last modified and its `etag`; `track_sources.external_id` is
-- the object's key. The local copy a load downloads is in the cache folder and isn't
-- recorded here: it's found by the URL (`bucket::Cache`).

-- `sources.kind` gains 's3'. SQLite can't alter a CHECK, so the table is made again.
-- (Migrations run with foreign keys off, so dropping the old table leaves
-- `track_sources` as it is.)
CREATE TABLE sources_new (
  id        INTEGER PRIMARY KEY,
  kind      TEXT NOT NULL CHECK (kind IN ('folder', 'rekordbox_db', 'rekordbox_xml',
                                          'rekordbox_usb', 'serato', 'serato_usb', 'mixxx',
                                          's3')),
  name      TEXT NOT NULL,
  location  BLOB,
  config    TEXT NOT NULL DEFAULT '{}',
  enabled   INTEGER NOT NULL DEFAULT 1,
  added_at  INTEGER NOT NULL,
  UNIQUE (kind, location)
) STRICT;
INSERT INTO sources_new(id, kind, name, location, config, enabled, added_at)
  SELECT id, kind, name, location, config, enabled, added_at FROM sources;
DROP TABLE sources;
ALTER TABLE sources_new RENAME TO sources;

-- A remote object's ETag, as its bucket listed it: with its size, how a rescan tells
-- the object has changed. NULL for a file on this computer.
ALTER TABLE files ADD COLUMN etag TEXT;
