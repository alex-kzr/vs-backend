mod adapters;
mod browser_review;
mod cv_reviews;
mod cvs;
pub mod error;
pub mod hh;
mod ingestion;
pub mod maintenance;
mod matching;
mod storage;
mod validation;
mod workflow;
mod writes;

use axum::{
    Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use error::{ApiError, Result};
use serde_json::{Value, json};
use sqlx::{
    Connection, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path as FilePath, time::Duration};

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub hosts: Vec<String>,
    pub origins: Vec<String>,
    pub hh: hh::HhClient,
}

pub async fn open_database(
    path: &FilePath,
) -> std::result::Result<SqlitePool, Box<dyn std::error::Error>> {
    open_database_with_creation(path, true).await
}
/// Open a managed file without creating a replacement at a wrong path.
pub async fn open_existing_database(
    path: &FilePath,
) -> std::result::Result<SqlitePool, Box<dyn std::error::Error>> {
    if !path.is_file() {
        return Err(
            std::io::Error::other("Existing database missing; no empty file created.").into(),
        );
    }
    open_database_with_creation(path, false).await
}
async fn open_database_with_creation(
    path: &FilePath,
    create: bool,
) -> std::result::Result<SqlitePool, Box<dyn std::error::Error>> {
    if create && let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .journal_mode(SqliteJournalMode::Delete)
        .synchronous(SqliteSynchronous::Full);
    // Table rebuilds require FK checks off before the migration transaction.
    // This connection is never exposed to HTTP handlers or returned to the pool.
    let migration_options = options.clone().foreign_keys(false);
    let mut migration = SqliteConnection::connect_with(&migration_options).await?;
    sqlx::migrate!().run(&mut migration).await?;
    if !sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut migration)
        .await?
        .is_empty()
    {
        return Err(
            std::io::Error::other("Database relationships failed integrity validation.").into(),
        );
    }
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut migration)
        .await?;
    let mut transaction = migration.begin().await?;
    storage::backfill_search(&mut transaction)
        .await
        .map_err(|e| std::io::Error::other(e.message))?;
    transaction.commit().await?;
    migration.close().await?;
    Ok(SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?)
}
pub fn router(state: AppState, assets: Option<&FilePath>) -> Router {
    let mut app = Router::new()
        .route("/api/v1/{*path}", any(api))
        .route("/api/health", any(health))
        .with_state(state);
    if let Some(path) = assets {
        app = app.fallback_service(tower_http::services::ServeDir::new(path).fallback(
            tower_http::services::ServeFile::new(path.join("index.html")),
        ));
    }
    app.layer(
        tower_http::set_header::SetResponseHeaderLayer::if_not_present(
            axum::http::header::X_CONTENT_TYPE_OPTIONS,
            axum::http::HeaderValue::from_static("nosniff"),
        ),
    )
}
fn boundary(state: &AppState, request: &Request) -> Result<()> {
    let host = request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !state.hosts.iter().any(|h| h == host) {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "invalid_host",
            "Use the configured loopback address.",
        ));
    }
    if let Some(origin) = request.headers().get("origin")
        && !origin
            .to_str()
            .ok()
            .is_some_and(|o| state.origins.iter().any(|a| a == o))
    {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "invalid_origin",
            "Origin is not allowed.",
        ));
    }
    Ok(())
}
async fn health(State(s): State<AppState>, request: Request) -> Result<Response> {
    boundary(&s, &request)?;
    if request.method() != Method::GET {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "GET required.",
        ));
    }
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&s.pool)
        .await?;
    Ok(axum::Json(json!({"status":"ok","sqlite_version":version})).into_response())
}
async fn api(
    State(s): State<AppState>,
    Path(path): Path<String>,
    request: Request,
) -> Result<Response> {
    boundary(&s, &request)?;
    let method = request.method().clone();
    let query = request.uri().query().unwrap_or("").to_owned();
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    if method == Method::GET && parts == ["hh", "status"] {
        return Ok(axum::Json(s.hh.status()).into_response());
    }
    if method == Method::DELETE && parts.len() == 3 && parts[..2] == ["hh", "requests"] {
        return Ok(axum::Json(s.hh.cancel(parts[2])?).into_response());
    }
    if method == Method::GET && parts == ["hh", "areas"] {
        return Ok(axum::Json(s.hh.areas(&query).await?).into_response());
    }
    if method == Method::GET
        && let ["search-configurations", id, "preview"] = parts.as_slice()
    {
        let args = hh::query::preview_query(&query)?;
        let configuration = {
            let mut transaction = s.pool.begin().await?;
            let data = storage::get(&mut transaction, "search_configuration", id).await?;
            transaction.commit().await?;
            data
        };
        let value = if args.adapter == "hh" {
            s.hh.preview(&configuration, args.page, args.request_id)
                .await?
        } else {
            adapters::preview(&configuration)?
        };
        return Ok(axum::Json(value).into_response());
    }
    if method == Method::GET {
        let mut connection = s.pool.begin().await?;
        let value = if workflow::handles(&parts) {
            workflow::read(&mut connection, &parts, &query).await?
        } else {
            storage::read(&mut connection, &parts, &query).await?
        };
        connection.commit().await?;
        return Ok(axum::Json(value).into_response());
    }
    if method != Method::POST && method != Method::PATCH {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Unsupported method.",
        ));
    }
    if method == Method::PATCH && workflow::handles(&parts) {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "Используйте создание новой версии или явное действие пакета.",
        ));
    }
    if request
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.split(';').next() != Some("application/json"))
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
            "Use application/json.",
        ));
    }
    let key = if method == Method::POST {
        let value = request
            .headers()
            .get("idempotency-key")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::field("Idempotency-Key", "A client UUID is required."))?;
        Some(
            uuid::Uuid::parse_str(value)
                .map_err(|_| ApiError::field("Idempotency-Key", "A client UUID is required."))?
                .to_string(),
        )
    } else {
        None
    };
    let bytes = to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Request exceeds 1 MiB.",
            )
        })?;
    let input: Value = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", "Invalid JSON."))?;
    let mut connection = s.pool.begin().await?;
    let (status, output) = if let Some(key) = key {
        if workflow::handles(&parts) {
            workflow::post(&mut connection, &parts, &key, input).await?
        } else {
            writes::create(&mut connection, &parts, &key, input).await?
        }
    } else {
        (
            StatusCode::OK,
            json!({"data":writes::patch(&mut connection,&parts,input).await?}),
        )
    };
    connection.commit().await?;
    Ok((status, axum::Json(output)).into_response())
}
