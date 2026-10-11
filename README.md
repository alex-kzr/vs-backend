# Vacancy Search Backend

Rust HTTP API, local SQLite persistence, immutable observations/messages, offline fixture search and a disabled-by-default official HH adapter. Axum 0.8.9, SQLx 0.9.0, Rust 1.96.0; exact transitive dependencies are in Cargo.lock.

## Run

From this repository:

```powershell
cargo run --locked
```

Default URL: http://127.0.0.1:48173. Build frontend first in ../frontend with npm ci and npm run build. Backend serves ../frontend/dist with SPA fallback. Database defaults to LOCALAPPDATA/VacancySearch/vacancy.sqlite3 (outside checkout). Optional process environment: VACANCY_DB (full file path), VACANCY_PORT (low-level override for isolated test servers only; user launch is fixed at 48173), VACANCY_ASSETS (frontend dist directory).

Startup creates the database, applies embedded checksum-checked migrations and backfills Unicode search projections before HTTP readiness. DELETE journal, FULL synchronous, FK enforcement, 5-second busy timeout and one SQLx pooled connection. Ctrl+C shuts down gracefully. /api/health returns the actual SQLite runtime version.

## Check

```powershell
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo build --release --locked
```

Tests use disposable file databases and synthetic data. They cover identity, retries, race, rollback, timeline high-water marks, upgrade/checksum/failure, integrity and stopped-copy restoration. HH tests use loopback HTTP mocks for headers/query/mapping, errors, limits, cooldown and cancellation. Current gate: 21 unit + 47 file-backed integration tests (including 21 CV/matching tests) pass, plus 3 Node transport tests.

## Boundaries

src/lib.rs owns HTTP extraction and same-origin/Host checks; validation.rs normalizes strict allowlists; storage.rs handles bound SQL, allowlisted identifiers and opaque cursor reads; writes.rs and ingestion.rs own transactional relationships/identity; adapters.rs implements pure fixture candidate acquisition/matching. SQLx 0.9 dynamic SQL uses AssertSqlSafe only for audited fixed identifiers; user values always use bind parameters.

Fixture-v1 is clearly synthetic. GET /api/v1/search-configurations/{id}/preview evaluates saved criteria without writes; selected imports use the ordinary ingestion POST. Companies are explicitly selected; names do not auto-merge.

The HH adapter is implemented in src/hh.rs and src/hh/. It permits only manually requested public search/region reads: fixed endpoint, timeout/body/page bounds, one request in flight, pacing, Retry-After and explicit cancellation. The DB transaction closes before awaiting HTTP. Optional source_payload preserves public source metadata in immutable discoveries. Local HTTP mocks test the real adapter; no real HH vacancy request was performed.

HH remains disabled unless HH_API_ACCESS_CONFIRMED=1 and a real registered-app HH_USER_AGENT are configured locally. HH_API_TOKEN is optional, if required for authorized access; no token value is exposed. Registration/applicant eligibility is unresolved, with the user reporting a company-only form. See ../../docs/architecture/hh.md and ../../docs/roadmap/hh-verification.md. No scheduler, outreach, remote access or communication account integration exists.

Shared API/architecture and current limitations are maintained in the umbrella docs. The authorized GitHub origin is https://github.com/alex-kzr/vs-backend. Publish checked commits normally before the umbrella gitlink; no force push.

## Refusal compatibility

Migration 8 preserves legacy rejected without inferring a party and adds declined_by_applicant / rejected_by_employer to API and SQLite validation. PATCH status changes append transactional history; GET /api/v1/vacancies/{id}/status-history reads it. Notes-only and same-status saves append no event. See ../../docs/architecture/localization-and-statuses.md.

## Multi-value filters

Lists support repeated status/company_id query keys for vacancies, and repeated company_id for contacts/conversations. OR within a selection, AND between filters. Empty selection omits the filter. Validation, canonical cursor binding, scalar URL compatibility and limits are documented in ../../docs/roadmap/multi-filters.md. No database migration is needed.

## CV and external-agent workflow

Migrations 9/10 add immutable CV revisions and explicitly requested matching batches/jobs/attempts. src/cvs.rs owns CV validation/versions, src/matching.rs pins inputs and validates external result evidence/state, src/workflow.rs persists replay receipts. There is no embedded semantic matcher, Codex process, scheduler or provider call. tools/agent.mjs is a loopback-only Node 24 transport with request envelopes persisted before POST. Run node --test tools/agent.test.mjs. The app accepts text and optional structured information; browser HH provenance is reported by an explicitly authorized external agent, with no login/cookie handling here. Full payloads, limits, retry behavior and examples: ../../docs/agent-matching.md. Tests use only synthetic CVs/results, and version-8 migration fixtures.

## CV lifecycle and origins

Migrations 11/12 add reversible deletion and source-scoped identities without rewriting legacy CV/revision JSON or receipts. POST cvs/{id}/delete and /restore require confirmation and current revision/lifecycle generation; retries are idempotent. New revisions and matching require active CVs; old pinned packets remain usable. GET cvs?deleted=true lists removed records; repeated source_code values select origins. GET cv-sources is read-only; new explicit domains register only on confirmed import. hh.ru/skillstaff.ru are metadata origins, distinct from revision.source.kind. No new site adapter is connected. Full contract: ../../docs/architecture/cv-lifecycle-and-sources.md.

CV state filters accept repeated cv_state=active / deleted with OR semantics, AND with source_code. Omitted state defaults to active; cv_state=all represents an empty UI selection. Legacy deleted=true/false links and their cursor scopes remain supported. Ambiguous legacy+new filters are rejected; all and both states share a cursor scope. No migration or data rewrite is required.
