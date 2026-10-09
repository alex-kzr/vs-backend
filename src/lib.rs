mod adapters;
pub mod error;
mod ingestion;
mod storage;
mod validation;
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
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path as FilePath, time::Duration};

#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub hosts: Vec<String>,
    pub origins: Vec<String>,
}

pub async fn open_database(
    path: &FilePath,
) -> std::result::Result<SqlitePool, Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .journal_mode(SqliteJournalMode::Delete)
        .synchronous(SqliteSynchronous::Full);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    let mut transaction = pool.begin().await?;
    storage::backfill_search(&mut transaction)
        .await
        .map_err(|e| std::io::Error::other(e.message))?;
    transaction.commit().await?;
    Ok(pool)
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
    if method == Method::GET {
        let mut connection = s.pool.begin().await?;
        let value = storage::read(&mut connection, &parts, &query).await?;
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
        writes::create(&mut connection, &parts, &key, input).await?
    } else {
        (
            StatusCode::OK,
            json!({"data":writes::patch(&mut connection,&parts,input).await?}),
        )
    };
    connection.commit().await?;
    Ok((status, axum::Json(output)).into_response())
}
