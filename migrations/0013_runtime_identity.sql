-- A durable identity distinguishes an installed DB from empty/redirected copies.
CREATE TABLE runtime_identity (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 instance_id TEXT NOT NULL UNIQUE CHECK(length(instance_id)=32),
 created_at TEXT NOT NULL
);
INSERT INTO runtime_identity VALUES(1,lower(hex(randomblob(16))),strftime('%Y-%m-%dT%H:%M:%fZ','now'));
