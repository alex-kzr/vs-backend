ALTER TABLE company ADD COLUMN normalized_text TEXT NOT NULL DEFAULT '';
ALTER TABLE contact ADD COLUMN normalized_text TEXT NOT NULL DEFAULT '';
ALTER TABLE vacancy ADD COLUMN normalized_text TEXT NOT NULL DEFAULT '';
CREATE INDEX company_created ON company(json_extract(data,'$.created_at') DESC,id DESC);
CREATE INDEX contact_created ON contact(json_extract(data,'$.created_at') DESC,id DESC);
CREATE INDEX vacancy_created ON vacancy(json_extract(data,'$.created_at') DESC,id DESC);
CREATE INDEX discovery_observed ON vacancy_discovery(vacancy_id,json_extract(data,'$.observed_at') DESC,id DESC);
