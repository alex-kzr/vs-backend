CREATE TABLE cv (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 hh_url TEXT UNIQUE,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE cv_revision (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 cv_id TEXT NOT NULL REFERENCES cv(id) ON DELETE RESTRICT,
 revision_number INTEGER NOT NULL CHECK(revision_number>0),
 data TEXT NOT NULL CHECK(json_valid(data)),
 UNIQUE(cv_id,revision_number), UNIQUE(id,cv_id)
);
CREATE INDEX cv_revision_parent ON cv_revision(cv_id,revision_number DESC);
CREATE TRIGGER cv_revision_immutable_update BEFORE UPDATE ON cv_revision BEGIN SELECT RAISE(ABORT,'CV revisions are immutable'); END;
CREATE TRIGGER cv_revision_immutable_delete BEFORE DELETE ON cv_revision BEGIN SELECT RAISE(ABORT,'CV revisions are immutable'); END;
