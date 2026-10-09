
CREATE TABLE company (ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE search_configuration (ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, name TEXT NOT NULL UNIQUE, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE vacancy_source (ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, code TEXT NOT NULL UNIQUE, data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE vacancy (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
 company_id TEXT NOT NULL REFERENCES company(id) ON DELETE RESTRICT,
 data TEXT NOT NULL CHECK(json_valid(data)), UNIQUE(id,company_id),
 CHECK(json_extract(data,'$.status') IN ('new','saved','contacted','rejected','archived'))
);
CREATE INDEX vacancy_company ON vacancy(company_id);
CREATE TABLE vacancy_source_reference (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
 vacancy_id TEXT NOT NULL REFERENCES vacancy(id) ON DELETE RESTRICT,
 source_id TEXT NOT NULL REFERENCES vacancy_source(id) ON DELETE RESTRICT,
 external_vacancy_id TEXT, normalized_url TEXT, data TEXT NOT NULL CHECK(json_valid(data)),
 CHECK(external_vacancy_id IS NOT NULL OR normalized_url IS NOT NULL), UNIQUE(id,vacancy_id)
);
CREATE UNIQUE INDEX reference_external_id ON vacancy_source_reference(source_id,external_vacancy_id) WHERE external_vacancy_id IS NOT NULL;
CREATE UNIQUE INDEX reference_url ON vacancy_source_reference(source_id,normalized_url) WHERE normalized_url IS NOT NULL;
CREATE INDEX reference_vacancy ON vacancy_source_reference(vacancy_id);
CREATE TABLE vacancy_discovery (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL,
 vacancy_id TEXT NOT NULL, source_reference_id TEXT NOT NULL,
 search_configuration_id TEXT REFERENCES search_configuration(id) ON DELETE RESTRICT,
 observation_key TEXT NOT NULL, data TEXT NOT NULL CHECK(json_valid(data)),
 FOREIGN KEY(source_reference_id,vacancy_id) REFERENCES vacancy_source_reference(id,vacancy_id) ON DELETE RESTRICT,
 UNIQUE(source_reference_id,observation_key)
);
CREATE INDEX discovery_vacancy ON vacancy_discovery(vacancy_id);
CREATE INDEX discovery_configuration ON vacancy_discovery(search_configuration_id);
CREATE TABLE request_receipt (
 operation TEXT NOT NULL, target TEXT NOT NULL, key TEXT NOT NULL,
 request_json TEXT NOT NULL CHECK(json_valid(request_json)),
 result_json TEXT NOT NULL CHECK(json_valid(result_json)),
 PRIMARY KEY(operation,target,key)
);
INSERT INTO vacancy_source(id,code,data) VALUES ('20000000-0000-4000-8000-000000000001','manual','{"id":"20000000-0000-4000-8000-000000000001","code":"manual","name":"Manual","enabled":true,"created_at":"2026-10-09T00:00:00.000Z","updated_at":"2026-10-09T00:00:00.000Z"}');
