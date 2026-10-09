
CREATE TABLE contact_role (ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,code TEXT UNIQUE NOT NULL,data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE communication_channel (ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,code TEXT UNIQUE NOT NULL,data TEXT NOT NULL CHECK(json_valid(data)));
CREATE TABLE contact (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,
 company_id TEXT NOT NULL REFERENCES company(id) ON DELETE RESTRICT,
 normalized_email TEXT, normalized_profile_url TEXT, data TEXT NOT NULL CHECK(json_valid(data)),
 UNIQUE(id,company_id)
);
CREATE UNIQUE INDEX contact_email ON contact(company_id,normalized_email) WHERE normalized_email IS NOT NULL;
CREATE UNIQUE INDEX contact_profile ON contact(company_id,normalized_profile_url) WHERE normalized_profile_url IS NOT NULL;
CREATE TABLE contact_role_assignment(contact_id TEXT NOT NULL REFERENCES contact(id) ON DELETE RESTRICT,role_id TEXT NOT NULL REFERENCES contact_role(id) ON DELETE RESTRICT,PRIMARY KEY(contact_id,role_id));
CREATE TABLE conversation (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,
 company_id TEXT NOT NULL REFERENCES company(id) ON DELETE RESTRICT,
 vacancy_id TEXT,channel_id TEXT NOT NULL REFERENCES communication_channel(id) ON DELETE RESTRICT,
 external_conversation_id TEXT,data TEXT NOT NULL CHECK(json_valid(data)),
 FOREIGN KEY(vacancy_id,company_id) REFERENCES vacancy(id,company_id) ON DELETE RESTRICT
);
CREATE UNIQUE INDEX conversation_external ON conversation(channel_id,external_conversation_id) WHERE external_conversation_id IS NOT NULL;
CREATE INDEX conversation_company ON conversation(company_id);
CREATE TABLE conversation_participant (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,
 conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE RESTRICT,
 participant_key TEXT NOT NULL,participant_kind TEXT NOT NULL CHECK(participant_kind IN ('user','contact','unknown')),
 contact_id TEXT REFERENCES contact(id) ON DELETE RESTRICT,data TEXT NOT NULL CHECK(json_valid(data)),
 CHECK((participant_kind='contact' AND contact_id IS NOT NULL) OR (participant_kind!='contact' AND contact_id IS NULL)),
 UNIQUE(conversation_id,participant_key),UNIQUE(id,conversation_id)
);
CREATE TABLE message (
 ingest_seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,
 conversation_id TEXT NOT NULL REFERENCES conversation(id) ON DELETE RESTRICT,
 sender_participant_id TEXT NOT NULL, external_message_id TEXT,client_message_id TEXT,
 data TEXT NOT NULL CHECK(json_valid(data)),
 FOREIGN KEY(sender_participant_id,conversation_id) REFERENCES conversation_participant(id,conversation_id) ON DELETE RESTRICT,
 CHECK(external_message_id IS NOT NULL OR client_message_id IS NOT NULL),
 CHECK(json_extract(data,'$.direction') IN ('inbound','outbound'))
);
CREATE UNIQUE INDEX message_external ON message(conversation_id,external_message_id) WHERE external_message_id IS NOT NULL;
CREATE UNIQUE INDEX message_client ON message(conversation_id,client_message_id) WHERE client_message_id IS NOT NULL;
CREATE INDEX message_timeline ON message(conversation_id,json_extract(data,'$.occurred_at'),id);
INSERT INTO communication_channel(id,code,data) VALUES ('a0000000-0000-4000-8000-000000000001','manual','{"id":"a0000000-0000-4000-8000-000000000001","code":"manual","label":"Manual record","created_at":"2026-10-09T00:00:00.000Z","updated_at":"2026-10-09T00:00:00.000Z"}');
INSERT INTO contact_role(id,code,data) VALUES
('90000000-0000-4000-8000-000000000001','recruiter','{"id":"90000000-0000-4000-8000-000000000001","code":"recruiter","label":"Recruiter","created_at":"2026-10-09T00:00:00.000Z","updated_at":"2026-10-09T00:00:00.000Z"}'),
('90000000-0000-4000-8000-000000000002','hiring_manager','{"id":"90000000-0000-4000-8000-000000000002","code":"hiring_manager","label":"Hiring manager","created_at":"2026-10-09T00:00:00.000Z","updated_at":"2026-10-09T00:00:00.000Z"}'),
('90000000-0000-4000-8000-000000000003','developer','{"id":"90000000-0000-4000-8000-000000000003","code":"developer","label":"Developer","created_at":"2026-10-09T00:00:00.000Z","updated_at":"2026-10-09T00:00:00.000Z"}'),
('90000000-0000-4000-8000-000000000004','other','{"id":"90000000-0000-4000-8000-000000000004","code":"other","label":"Other","created_at":"2026-10-09T00:00:00.000Z","updated_at":"2026-10-09T00:00:00.000Z"}');
