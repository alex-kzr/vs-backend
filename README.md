# Vacancy Search Backend

Rust HTTP API, local SQLite persistence, immutable observations/messages, offline fixture search and a disabled-by-default official HH adapter. Axum 0.8.9, SQLx 0.9.0, Rust 1.96.0; exact transitive dependencies are in Cargo.lock.

## Run

From this repository:

```powershell
cargo run --locked
```

Default URL: http://127.0.0.1:48173. Build frontend first in ../frontend with npm ci and npm run build. Backend serves ../frontend/dist with SPA fallback. Database defaults to LOCALAPPDATA/VacancySearch/vacancy.sqlite3 (outside checkout). Optional process environment: VACANCY_DB (full file path), VACANCY_PORT (u16 loopback port), VACANCY_ASSETS (frontend dist directory).

Startup creates the database, applies embedded checksum-checked migrations and backfills Unicode search projections before HTTP readiness. DELETE journal, FULL synchronous, FK enforcement, 5-second busy timeout and one SQLx pooled connection. Ctrl+C shuts down gracefully. /api/health returns the actual SQLite runtime version.

## Check

```powershell
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo build --release --locked
```

Tests use disposable file databases and synthetic data. They cover identity, retries, race, rollback, timeline high-water marks, upgrade/checksum/failure, integrity and stopped-copy restoration. HH tests use loopback HTTP mocks for headers/query/mapping, errors, limits, cooldown and cancellation. Current gate: 17 unit + 19 file-backed integration tests pass.

## Boundaries

src/lib.rs owns HTTP extraction and same-origin/Host checks; validation.rs normalizes strict allowlists; storage.rs handles bound SQL, allowlisted identifiers and opaque cursor reads; writes.rs and ingestion.rs own transactional relationships/identity; adapters.rs implements pure fixture candidate acquisition/matching. SQLx 0.9 dynamic SQL uses AssertSqlSafe only for audited fixed identifiers; user values always use bind parameters.

Fixture-v1 is clearly synthetic. GET /api/v1/search-configurations/{id}/preview evaluates saved criteria without writes; selected imports use the ordinary ingestion POST. Companies are explicitly selected; names do not auto-merge.

The HH adapter is implemented in src/hh.rs and src/hh/. It permits only manually requested public search/region reads: fixed endpoint, timeout/body/page bounds, one request in flight, pacing, Retry-After and explicit cancellation. The DB transaction closes before awaiting HTTP. Optional source_payload preserves public source metadata in immutable discoveries. Local HTTP mocks test the real adapter; no real HH vacancy request was performed.

HH remains disabled unless HH_API_ACCESS_CONFIRMED=1 and a real registered-app HH_USER_AGENT are configured locally. HH_API_TOKEN is optional, if required for authorized access; no token value is exposed. Registration/applicant eligibility is unresolved, with the user reporting a company-only form. See ../../docs/architecture/hh.md and ../../docs/roadmap/hh-verification.md. No scheduler, outreach, remote access or communication account integration exists.

Shared API/architecture and current limitations are maintained in the umbrella docs. Application commits are local and unpublished; no upstream remote is configured.
