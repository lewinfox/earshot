-- Saved Flows, in folders. A playlist is a Flow whose cards form one chain.
CREATE TABLE flow_folders (
  id         INTEGER PRIMARY KEY,
  parent_id  INTEGER REFERENCES flow_folders(id) ON DELETE CASCADE,  -- NULL: top level
  name       TEXT NOT NULL
);

CREATE TABLE flows (
  id          INTEGER PRIMARY KEY,
  folder_id   INTEGER REFERENCES flow_folders(id) ON DELETE CASCADE, -- NULL: top level
  name        TEXT NOT NULL,
  created_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- One row per card. `parent_card` is the card this one follows (NULL: a starting card).
CREATE TABLE flow_cards (
  flow_id      INTEGER NOT NULL REFERENCES flows(id) ON DELETE CASCADE,
  card_id      INTEGER NOT NULL,
  track_id     INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  parent_card  INTEGER,
  x            REAL NOT NULL,
  y            REAL NOT NULL,
  PRIMARY KEY (flow_id, card_id)
);
