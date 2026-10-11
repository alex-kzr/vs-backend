CREATE TABLE match_run (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 cv_id TEXT NOT NULL REFERENCES cv(id) ON DELETE RESTRICT,
 cv_revision_id TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data)),
 FOREIGN KEY(cv_revision_id,cv_id) REFERENCES cv_revision(id,cv_id) ON DELETE RESTRICT
);
CREATE TABLE match_job (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 run_id TEXT NOT NULL REFERENCES match_run(id) ON DELETE RESTRICT,
 vacancy_id TEXT NOT NULL REFERENCES vacancy(id) ON DELETE RESTRICT,
 data TEXT NOT NULL CHECK(json_valid(data)),
 UNIQUE(run_id,vacancy_id), UNIQUE(id,run_id)
);
CREATE INDEX match_job_run ON match_job(run_id,ingest_seq);
CREATE TABLE match_attempt (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 job_id TEXT NOT NULL REFERENCES match_job(id) ON DELETE RESTRICT,
 attempt_number INTEGER NOT NULL CHECK(attempt_number BETWEEN 1 AND 100),
 data TEXT NOT NULL CHECK(json_valid(data)),
 UNIQUE(job_id,attempt_number)
);
CREATE INDEX match_attempt_job ON match_attempt(job_id,attempt_number);
CREATE TRIGGER match_attempt_closed_immutable BEFORE UPDATE ON match_attempt
 WHEN json_extract(OLD.data,'$.state') IN ('completed','failed','cancelled')
 BEGIN SELECT RAISE(ABORT,'Closed matching attempts are immutable'); END;
CREATE TRIGGER match_attempt_immutable_delete BEFORE DELETE ON match_attempt BEGIN SELECT RAISE(ABORT,'Matching attempts are immutable'); END;
