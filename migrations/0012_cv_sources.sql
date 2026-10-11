CREATE TABLE cv_source (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,
 id TEXT NOT NULL UNIQUE, code TEXT NOT NULL UNIQUE,
 data TEXT NOT NULL CHECK(json_valid(data))
);
INSERT INTO cv_source(id,code,data) VALUES
('60000000-0000-4000-8000-000000000001','manual','{"id":"60000000-0000-4000-8000-000000000001","code":"manual","label":"Вручную","created_at":"2026-10-11T00:00:00Z","updated_at":"2026-10-11T00:00:00Z"}'),
('60000000-0000-4000-8000-000000000002','hh.ru','{"id":"60000000-0000-4000-8000-000000000002","code":"hh.ru","label":"hh.ru","created_at":"2026-10-11T00:00:00Z","updated_at":"2026-10-11T00:00:00Z"}'),
('60000000-0000-4000-8000-000000000003','skillstaff.ru','{"id":"60000000-0000-4000-8000-000000000003","code":"skillstaff.ru","label":"skillstaff.ru","created_at":"2026-10-11T00:00:00Z","updated_at":"2026-10-11T00:00:00Z"}');
ALTER TABLE cv ADD COLUMN source_code TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE cv ADD COLUMN source_url TEXT;
ALTER TABLE cv ADD COLUMN external_id TEXT;
UPDATE cv SET source_code='hh.ru',source_url=hh_url,
 external_id=substr(hh_url,length('https://hh.ru/resume/')+1) WHERE hh_url IS NOT NULL;
CREATE UNIQUE INDEX cv_source_url_identity ON cv(source_code,source_url) WHERE source_url IS NOT NULL;
CREATE UNIQUE INDEX cv_external_identity ON cv(source_code,external_id) WHERE external_id IS NOT NULL;
CREATE INDEX cv_source_scope ON cv(source_code,deleted_at,ingest_seq);
CREATE TRIGGER cv_source_valid_insert BEFORE INSERT ON cv WHEN NOT EXISTS(SELECT 1 FROM cv_source WHERE code=NEW.source_code)
 BEGIN SELECT RAISE(ABORT,'CV source must be registered'); END;
CREATE TRIGGER cv_source_valid_update BEFORE UPDATE OF source_code ON cv WHEN NOT EXISTS(SELECT 1 FROM cv_source WHERE code=NEW.source_code)
 BEGIN SELECT RAISE(ABORT,'CV source must be registered'); END;
