CREATE TABLE cv_review_run (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 cv_id TEXT NOT NULL REFERENCES cv(id) ON DELETE RESTRICT,
 cv_revision_id TEXT NOT NULL,
 reference_cv_id TEXT REFERENCES cv(id) ON DELETE RESTRICT,
 reference_revision_id TEXT,
 scope TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data)),
 FOREIGN KEY(cv_revision_id,cv_id) REFERENCES cv_revision(id,cv_id) ON DELETE RESTRICT,
 FOREIGN KEY(reference_revision_id,reference_cv_id) REFERENCES cv_revision(id,cv_id) ON DELETE RESTRICT,
 CHECK((reference_cv_id IS NULL)=(reference_revision_id IS NULL)),
 CHECK(reference_cv_id IS NULL OR reference_cv_id<>cv_id)
);
CREATE INDEX cv_review_run_scope ON cv_review_run(scope,ingest_seq DESC);
CREATE INDEX cv_review_run_cv ON cv_review_run(cv_id,ingest_seq DESC);
CREATE TRIGGER cv_review_run_closed BEFORE UPDATE ON cv_review_run
 WHEN json_extract(OLD.data,'$.state') IN ('completed','cancelled')
 BEGIN SELECT RAISE(ABORT,'Closed CV reviews are immutable'); END;
CREATE TABLE cv_recommendation (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 cv_id TEXT NOT NULL REFERENCES cv(id) ON DELETE RESTRICT,
 reference_cv_id TEXT REFERENCES cv(id) ON DELETE RESTRICT,
 scope TEXT NOT NULL,
 issue_key TEXT NOT NULL,
 data TEXT NOT NULL CHECK(json_valid(data)),
 UNIQUE(scope,issue_key)
);
CREATE INDEX cv_recommendation_cv ON cv_recommendation(cv_id,ingest_seq DESC);
CREATE TABLE cv_review_event (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 run_id TEXT NOT NULL REFERENCES cv_review_run(id) ON DELETE RESTRICT,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TABLE cv_recommendation_event (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 recommendation_id TEXT NOT NULL REFERENCES cv_recommendation(id) ON DELETE RESTRICT,
 run_id TEXT NOT NULL REFERENCES cv_review_run(id) ON DELETE RESTRICT,
 data TEXT NOT NULL CHECK(json_valid(data))
);
CREATE TRIGGER cv_review_event_update BEFORE UPDATE ON cv_review_event BEGIN SELECT RAISE(ABORT,'Review history is immutable'); END;
CREATE TRIGGER cv_review_event_delete BEFORE DELETE ON cv_review_event BEGIN SELECT RAISE(ABORT,'Review history is immutable'); END;
CREATE TRIGGER cv_recommendation_event_update BEFORE UPDATE ON cv_recommendation_event BEGIN SELECT RAISE(ABORT,'Recommendation history is immutable'); END;
CREATE TRIGGER cv_recommendation_event_delete BEFORE DELETE ON cv_recommendation_event BEGIN SELECT RAISE(ABORT,'Recommendation history is immutable'); END;
CREATE TRIGGER cv_review_run_delete BEFORE DELETE ON cv_review_run BEGIN SELECT RAISE(ABORT,'Reviews cannot be deleted'); END;
CREATE TRIGGER cv_recommendation_delete BEFORE DELETE ON cv_recommendation BEGIN SELECT RAISE(ABORT,'Recommendations cannot be deleted'); END;

CREATE TABLE cv_review_vacancy (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE,
 run_id TEXT NOT NULL REFERENCES cv_review_run(id) ON DELETE RESTRICT,
 vacancy_id TEXT NOT NULL REFERENCES vacancy(id) ON DELETE RESTRICT,
 data TEXT NOT NULL CHECK(json_valid(data)),
 UNIQUE(run_id,vacancy_id)
);
CREATE TRIGGER cv_review_vacancy_update BEFORE UPDATE ON cv_review_vacancy BEGIN SELECT RAISE(ABORT,'Review inputs are immutable'); END;
CREATE TRIGGER cv_review_vacancy_delete BEFORE DELETE ON cv_review_vacancy BEGIN SELECT RAISE(ABORT,'Review inputs are immutable'); END;
