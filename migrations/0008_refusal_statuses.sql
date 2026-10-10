-- Rebuild using SQLite's create/copy/drop/rename procedure on the startup-only
-- migration connection. Runtime connections always enforce foreign keys.
CREATE TEMP TABLE vacancy_sequence_before AS SELECT seq FROM sqlite_sequence WHERE name='vacancy';
CREATE TABLE vacancy_status_upgrade (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
 company_id TEXT NOT NULL REFERENCES company(id) ON DELETE RESTRICT,
 data TEXT NOT NULL CHECK(json_valid(data)),
 normalized_text TEXT NOT NULL DEFAULT '', UNIQUE(id,company_id),
 CHECK(COALESCE(json_type(data,'$.status')='text' AND json_extract(data,'$.status') IN
 ('new','saved','contacted','declined_by_applicant','rejected_by_employer','rejected','archived'),0))
);
INSERT INTO vacancy_status_upgrade(ingest_seq,id,company_id,data,normalized_text)
 SELECT ingest_seq,id,company_id,data,normalized_text FROM vacancy;
DROP TABLE vacancy;
ALTER TABLE vacancy_status_upgrade RENAME TO vacancy;
INSERT INTO sqlite_sequence(name,seq)
 SELECT 'vacancy',seq FROM vacancy_sequence_before
 WHERE NOT EXISTS(SELECT 1 FROM sqlite_sequence WHERE name='vacancy');
UPDATE sqlite_sequence SET seq=MAX(seq,COALESCE((SELECT seq FROM vacancy_sequence_before),seq)) WHERE name='vacancy';
DROP TABLE vacancy_sequence_before;
CREATE INDEX vacancy_company ON vacancy(company_id);
CREATE INDEX vacancy_created ON vacancy(json_extract(data,'$.created_at') DESC,id DESC);
CREATE TABLE vacancy_status_history (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
 vacancy_id TEXT NOT NULL REFERENCES vacancy(id) ON DELETE RESTRICT,
 data TEXT NOT NULL CHECK(json_valid(data)),
 CHECK(COALESCE(json_extract(data,'$.previous_status') IN
 ('new','saved','contacted','declined_by_applicant','rejected_by_employer','rejected','archived'),0)),
 CHECK(COALESCE(json_extract(data,'$.status') IN
 ('new','saved','contacted','declined_by_applicant','rejected_by_employer','rejected','archived'),0))
);
CREATE INDEX status_history_changed ON vacancy_status_history(vacancy_id,json_extract(data,'$.changed_at') DESC,id DESC);
-- Fail inside the same migration transaction if any association was broken.
CREATE TEMP TABLE vacancy_fk_guard(valid INTEGER CHECK(valid=1));
INSERT INTO vacancy_fk_guard SELECT 0 FROM pragma_foreign_key_check;
DROP TABLE vacancy_fk_guard;
