ALTER TABLE cv ADD COLUMN deleted_at TEXT;
ALTER TABLE cv ADD COLUMN lifecycle_version INTEGER NOT NULL DEFAULT 0 CHECK(lifecycle_version>=0);
CREATE INDEX cv_active ON cv(deleted_at,ingest_seq);
