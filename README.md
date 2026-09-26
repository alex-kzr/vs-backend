# Vacancy Search Backend

Rust HTTP API and SQLite persistence for Vacancy Search. This repository is prepared for application implementation; no application code or build manifest exists yet.

## Ownership

Backend owns source, SQL migrations, dependency locks and backend tests here. The umbrella repository owns the shared API contract and architecture: https://github.com/alex-kzr/vacancy-search/tree/main/docs

Intended submodule path: `repos/backend`. Implement against the umbrella API and identity rules; verify the toolchain and dependency versions before adoption.
