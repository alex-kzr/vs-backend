//! Local maintenance CLI. Reports contain counts/hashes, never CVs or contacts.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    Column, Connection, Row, SqliteConnection, TypeInfo, ValueRef, sqlite::SqliteConnectOptions,
};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io,
    path::Path,
    time::{Duration, Instant},
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

fn error(message: impl Into<String>) -> Box<dyn std::error::Error> {
    io::Error::other(message.into()).into()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
async fn existing(path: &Path) -> Result<SqliteConnection> {
    if !path.is_file() {
        return Err(error(
            "Database file missing; no empty database was created.",
        ));
    }
    Ok(SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .read_only(true)
            .busy_timeout(Duration::from_secs(2)),
    )
    .await?)
}
async fn columns(c: &mut SqliteConnection, table: &str) -> Result<Vec<String>> {
    Ok(
        sqlx::query("SELECT name FROM pragma_table_info(?) ORDER BY cid")
            .bind(table)
            .fetch_all(&mut *c)
            .await?
            .iter()
            .map(|r| r.try_get("name"))
            .collect::<std::result::Result<_, _>>()?,
    )
}
async fn rows(
    c: &mut SqliteConnection,
    table: &str,
    cols: &[String],
) -> Result<BTreeMap<String, usize>> {
    let query = format!(
        "SELECT {} FROM {}",
        cols.iter().map(|s| quoted(s)).collect::<Vec<_>>().join(","),
        quoted(table)
    );
    let mut values = BTreeMap::new();
    // Only escaped, double-quoted schema identifiers enter this audited SQL.
    for row in sqlx::query(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(&mut *c)
        .await?
    {
        let mut data = Vec::new();
        for col in row.columns() {
            let raw = row.try_get_raw(col.ordinal())?;
            data.push(if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "INTEGER" | "INT" | "BOOLEAN" => json!(row.try_get::<i64, _>(col.ordinal())?),
                    "REAL" | "FLOAT" => json!(row.try_get::<f64, _>(col.ordinal())?),
                    "BLOB" => json!({"blob":hex(&row.try_get::<Vec<u8>,_>(col.ordinal())?)}),
                    _ => json!(row.try_get::<String, _>(col.ordinal())?),
                }
            });
        }
        *values.entry(serde_json::to_string(&data)?).or_insert(0) += 1;
    }
    Ok(values)
}
/// Embedded migration versions and SHA384 checksums for release manifests.
pub fn release_info() -> Value {
    json!({"format":1,"migrations":MIGRATOR.iter().map(|m|json!({"version":m.version,"checksum":hex(&m.checksum)})).collect::<Vec<_>>()})
}
/// Read a consistent inspection snapshot. Personal record values are hashed.
pub async fn inspect(path: &Path) -> Result<Value> {
    let mut connection = existing(path).await?;
    let mut transaction = connection.begin().await?;
    let c = &mut *transaction;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&mut *c)
        .await?;
    if integrity != "ok"
        || !sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *c)
            .await?
            .is_empty()
    {
        return Err(error("Database integrity/foreign-key check failed."));
    }
    let schema: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT type,name,sql FROM sqlite_master ORDER BY type,name")
            .fetch_all(&mut *c)
            .await?;
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .fetch_all(&mut *c)
            .await?;
    let mut tables = BTreeMap::new();
    for name in &names {
        let cols = columns(c, name).await?;
        let data = rows(c, name, &cols).await?;
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(
            format!("SELECT count(*) FROM {}", quoted(name)).as_str(),
        ))
        .fetch_one(&mut *c)
        .await?;
        tables.insert(name.clone(),json!({"rows":count,"columns":cols,"sha256":hash(serde_json::to_string(&data)?.as_bytes())}));
    }
    let installed: Vec<(i64, Vec<u8>, bool)> = if names.iter().any(|n| n == "_sqlx_migrations") {
        sqlx::query_as("SELECT version,checksum,success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *c)
            .await?
    } else {
        Vec::new()
    };
    let expected: Vec<_> = MIGRATOR.iter().collect();
    let valid = installed.len() <= expected.len()
        && installed
            .iter()
            .zip(&expected)
            .all(|((v, checksum, success), m)| {
                *v == m.version && checksum.as_slice() == m.checksum.as_ref() && *success
            });
    let pending: Vec<i64> = expected
        .iter()
        .filter(|m| !installed.iter().any(|(v, _, _)| *v == m.version))
        .map(|m| m.version)
        .collect();
    let instance: Option<String> = if names.iter().any(|n| n == "runtime_identity") {
        sqlx::query_scalar("SELECT instance_id FROM runtime_identity WHERE singleton=1")
            .fetch_optional(&mut *c)
            .await?
    } else {
        None
    };
    let fingerprint =
        hash(serde_json::to_string(&json!({"schema":schema,"tables":tables}))?.as_bytes());
    transaction.commit().await?;
    connection.close().await?;
    Ok(
        json!({"canonical_path":std::fs::canonicalize(path)?,"fingerprint":fingerprint,"integrity":"ok","foreign_keys":"ok","tables":tables,"migrations":installed.iter().map(|(v,sum,success)|json!({"version":v,"checksum":hex(sum),"success":success})).collect::<Vec<_>>(),"migrations_valid":valid,"compatible":valid&&pending.is_empty(),"pending_migrations":pending,"instance_id":instance}),
    )
}
/// SQLite online backup API; source WAL participates, destinations never overwrite.
pub async fn backup(source: &Path, destination: &Path) -> Result<Value> {
    let mut src = existing(source).await?;
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let mut dst = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(destination)
            .create_if_missing(false)
            .busy_timeout(Duration::from_secs(2)),
    )
    .await?;
    {
        let mut source_handle = src.lock_handle().await?;
        let mut destination_handle = dst.lock_handle().await?;
        let source_ptr = source_handle.as_raw_handle().as_ptr();
        let destination_ptr = destination_handle.as_raw_handle().as_ptr();
        // SAFETY: both SQLx worker handles stay exclusively locked and alive until finish.
        let backup = unsafe {
            libsqlite3_sys::sqlite3_backup_init(
                destination_ptr,
                c"main".as_ptr(),
                source_ptr,
                c"main".as_ptr(),
            )
        };
        if backup.is_null() {
            return Err(error(
                "SQLite backup initialization failed; source/destination unavailable.",
            ));
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        let step = loop {
            // SAFETY: backup is live/non-null and both connection locks remain held.
            let status = unsafe { libsqlite3_sys::sqlite3_backup_step(backup, 128) };
            if status == libsqlite3_sys::SQLITE_DONE {
                break status;
            }
            if ![
                libsqlite3_sys::SQLITE_OK,
                libsqlite3_sys::SQLITE_BUSY,
                libsqlite3_sys::SQLITE_LOCKED,
            ]
            .contains(&status)
                || Instant::now() >= deadline
            {
                break status;
            }
            if status != libsqlite3_sys::SQLITE_OK {
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // SAFETY: finish exactly once before either connection/lock is dropped.
        let finish = unsafe { libsqlite3_sys::sqlite3_backup_finish(backup) };
        if step != libsqlite3_sys::SQLITE_DONE || finish != libsqlite3_sys::SQLITE_OK {
            return Err(error(format!(
                "SQLite backup incomplete (status {step}, finish {finish}); partial destination retained."
            )));
        }
    }
    dst.close().await?;
    src.close().await?;
    inspect(destination).await
}
/// Verify every preexisting row in every preexisting column is still present.
/// New migrations that intentionally rewrite old values need an audited contract.
pub async fn verify_preservation(before: &Path, after: &Path) -> Result<Value> {
    let mut source = existing(before).await?;
    let mut target = existing(after).await?;
    let mut source_tx = source.begin().await?;
    let mut target_tx = target.begin().await?;
    let names:Vec<String>=sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' AND name NOT IN ('_sqlx_migrations','sqlite_sequence') ORDER BY name").fetch_all(&mut *source_tx).await?;
    for name in &names {
        let cols = columns(&mut target_tx, name).await?;
        let previous_cols = columns(&mut source_tx, name).await?;
        if !previous_cols.iter().all(|col| cols.contains(col)) {
            return Err(error(format!("Migration removed prior columns in {name}.")));
        }
        let previous = rows(&mut source_tx, name, &previous_cols).await?;
        let next = rows(&mut target_tx, name, &previous_cols).await?;
        if !previous
            .iter()
            .all(|(row, count)| next.get(row).is_some_and(|present| present >= count))
        {
            return Err(error(format!(
                "Migration changed/removed prior rows in {name}; activation refused."
            )));
        }
    }
    source_tx.commit().await?;
    target_tx.commit().await?;
    source.close().await?;
    target.close().await?;
    Ok(json!({"prior_rows_preserved":true,"tables_checked":names.len()}))
}
/// Apply this executable's migrations to an explicitly existing file only.
pub async fn migrate_existing(path: &Path) -> Result<Value> {
    let before = inspect(path).await?;
    if before["migrations_valid"] != true {
        return Err(error(
            "Migration checksums/versions are incompatible; database unchanged.",
        ));
    }
    let pool = crate::open_existing_database(path).await?;
    pool.close().await;
    inspect(path).await
}
/// OS-owned lifetime lock; stale filenames are harmless after handles close.
pub fn runtime_lock(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(path).map_err(|_| {
        error("Runtime lock is busy or inaccessible; another launch/update may be running.")
    })?;
    file.try_lock()
        .map_err(|_| error("Runtime lock is already owned."))?;
    Ok(file)
}
/// Refuse redirected copies, missing files, unexpected identity or schema.
pub async fn validate_managed(path: &Path, physical: &Path, instance: &str) -> Result<()> {
    if std::fs::canonicalize(path)? != physical {
        return Err(error(
            "Physical database path differs from installation; activation refused.",
        ));
    }
    let report = inspect(path).await?;
    if report["instance_id"].as_str() != Some(instance)
        || instance.is_empty()
        || report["compatible"] != true
    {
        return Err(error(
            "Managed database identity/schema mismatch; use the verified updater.",
        ));
    }
    Ok(())
}
/// Parse maintenance commands without shell interpretation.
pub async fn cli(arguments: &[std::ffi::OsString]) -> Result<()> {
    let Some(operation) = arguments.first().and_then(|s| s.to_str()) else {
        return Err(error("Maintenance operation required."));
    };
    if operation == "release-info" && arguments.len() == 1 {
        println!("{}", release_info());
        return Ok(());
    }
    let mut database = None;
    let mut output = None;
    let mut baseline = None;
    for pair in arguments[1..].chunks(2) {
        if pair.len() != 2 {
            return Err(error("Maintenance flag needs a path."));
        }
        let target = match pair[0].to_str() {
            Some("--database") => &mut database,
            Some("--output") => &mut output,
            Some("--baseline") => &mut baseline,
            _ => return Err(error("Unknown maintenance flag.")),
        };
        if target.is_some() {
            return Err(error("Duplicate maintenance flag."));
        }
        *target = Some(std::path::PathBuf::from(&pair[1]));
    }
    let database = database.ok_or_else(|| error("Existing --database is required."))?;
    let report = match operation {
        "inspect" if output.is_none() && baseline.is_none() => inspect(&database).await?,
        "migrate" if output.is_none() && baseline.is_none() => migrate_existing(&database).await?,
        "backup" if baseline.is_none() => {
            backup(
                &database,
                &output.ok_or_else(|| error("New --output required."))?,
            )
            .await?
        }
        "preserve" if output.is_none() => {
            verify_preservation(
                &baseline.ok_or_else(|| error("--baseline required."))?,
                &database,
            )
            .await?
        }
        _ => return Err(error("Invalid maintenance operation/flags.")),
    };
    println!("{report}");
    Ok(())
}
