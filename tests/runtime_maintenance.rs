use serde_json::Value;
use sqlx::{
    Connection, SqliteConnection,
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
};
use vacancy_search_backend::{maintenance, open_database};

#[tokio::test]
async fn backup_captures_committed_live_wal_and_refuses_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("live.sqlite3");
    let output = dir.path().join("backup.sqlite3");
    let mut live = SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(&source)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal),
    )
    .await
    .unwrap();
    sqlx::query("CREATE TABLE private_notes(id INTEGER PRIMARY KEY, note TEXT NOT NULL)")
        .execute(&mut live)
        .await
        .unwrap();
    sqlx::query("INSERT INTO private_notes VALUES(1,'synthetic private note')")
        .execute(&mut live)
        .await
        .unwrap();
    assert!(dir.path().join("live.sqlite3-wal").is_file());
    let report = maintenance::backup(&source, &output).await.unwrap();
    assert_eq!(report["tables"]["private_notes"]["rows"], 1);
    let baseline = std::fs::read(&output).unwrap();
    assert!(maintenance::backup(&source, &output).await.is_err());
    assert_eq!(std::fs::read(&output).unwrap(), baseline);
    live.close().await.unwrap();
}
#[tokio::test]
async fn missing_source_never_creates_an_empty_database() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sqlite3");
    let output = dir.path().join("backup.sqlite3");
    assert!(maintenance::backup(&missing, &output).await.is_err());
    assert!(maintenance::migrate_existing(&missing).await.is_err());
    assert!(!missing.exists());
    assert!(!output.exists());
}
#[tokio::test]
async fn inspection_does_not_emit_personal_values_or_change_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite3");
    let pool = open_database(&path).await.unwrap();
    sqlx::query("CREATE TABLE private_notes(id INTEGER PRIMARY KEY,note TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO private_notes VALUES(1,'synthetic-private-value-do-not-report')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let before = std::fs::read(&path).unwrap();
    let report = maintenance::inspect(&path).await.unwrap();
    assert!(
        !report
            .to_string()
            .contains("synthetic-private-value-do-not-report")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(report["compatible"], true);
    assert_eq!(report["instance_id"].as_str().unwrap().len(), 32);
}
#[test]
fn runtime_lock_blocks_a_second_owner_and_recovers_after_exit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.lock");
    let owner = maintenance::runtime_lock(&path).unwrap();
    assert!(maintenance::runtime_lock(&path).is_err());
    drop(owner);
    assert!(maintenance::runtime_lock(&path).is_ok());
}
#[tokio::test]
async fn migration_refuses_modified_checksum_and_keeps_input_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite3");
    let pool = open_database(&path).await.unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=1")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let before = std::fs::read(&path).unwrap();
    assert!(maintenance::migrate_existing(&path).await.is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
#[tokio::test]
async fn staged_identity_migration_preserves_the_source_and_prior_rows() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("legacy.sqlite3");
    let output = dir.path().join("staged.sqlite3");
    let pool = open_database(&source).await.unwrap();
    sqlx::query("CREATE TABLE private_notes(id INTEGER PRIMARY KEY,note TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO private_notes VALUES(1,'synthetic preserved note')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DROP TABLE runtime_identity")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version=13")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let baseline = maintenance::inspect(&source).await.unwrap();
    assert_eq!(baseline["pending_migrations"], serde_json::json!([13]));
    maintenance::backup(&source, &output).await.unwrap();
    let migrated = maintenance::migrate_existing(&output).await.unwrap();
    let untouched = maintenance::inspect(&source).await.unwrap();
    assert_eq!(untouched["fingerprint"], baseline["fingerprint"]);
    assert_eq!(
        migrated["tables"]["private_notes"],
        baseline["tables"]["private_notes"]
    );
    assert_eq!(migrated["compatible"], Value::Bool(true));
}

#[tokio::test]
async fn managed_launch_rejects_redirected_path_and_wrong_identity_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite3");
    open_database(&path).await.unwrap().close().await;
    let before = maintenance::inspect(&path).await.unwrap();
    let physical = std::fs::canonicalize(&path).unwrap();
    let instance = before["instance_id"].as_str().unwrap();
    assert!(
        maintenance::validate_managed(&path, &physical, instance)
            .await
            .is_ok()
    );
    assert!(
        maintenance::validate_managed(&path, &dir.path().join("other.sqlite3"), instance)
            .await
            .is_err()
    );
    assert!(
        maintenance::validate_managed(&path, &physical, "wrong")
            .await
            .is_err()
    );
    assert_eq!(
        maintenance::inspect(&path).await.unwrap()["fingerprint"],
        before["fingerprint"]
    );
}
#[tokio::test]
async fn preservation_refuses_duplicate_row_loss() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("before.sqlite3");
    let target = dir.path().join("after.sqlite3");
    let pool = open_database(&source).await.unwrap();
    sqlx::query("CREATE TABLE private_notes(note TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO private_notes VALUES('same'),('same')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    maintenance::backup(&source, &target).await.unwrap();
    let mut c = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
        .await
        .unwrap();
    sqlx::query("DELETE FROM private_notes WHERE rowid=1")
        .execute(&mut c)
        .await
        .unwrap();
    c.close().await.unwrap();
    assert!(
        maintenance::verify_preservation(&source, &target)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn schema_identifiers_are_quoted_without_sql_injection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.sqlite3");
    let pool = open_database(&path).await.unwrap();
    sqlx::query(r#"CREATE TABLE "notes""; DROP TABLE cv; --"(id INTEGER)"#)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(r#"INSERT INTO "notes""; DROP TABLE cv; --" VALUES(1)"#)
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    let inspection = maintenance::inspect(&path).await.unwrap();
    assert_eq!(
        inspection["tables"][r#"notes"; DROP TABLE cv; --"#]["rows"],
        1
    );
    assert!(inspection["tables"].get("cv").is_some());
}
#[tokio::test]
async fn preservation_refuses_rewritten_note_content() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("before.sqlite3");
    let target = dir.path().join("after.sqlite3");
    let pool = open_database(&source).await.unwrap();
    sqlx::query("CREATE TABLE private_notes(id INTEGER PRIMARY KEY,note TEXT)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO private_notes VALUES(1,'old note')")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
    maintenance::backup(&source, &target).await.unwrap();
    let mut c = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&target))
        .await
        .unwrap();
    sqlx::query("UPDATE private_notes SET note='different note'")
        .execute(&mut c)
        .await
        .unwrap();
    c.close().await.unwrap();
    assert!(
        maintenance::verify_preservation(&source, &target)
            .await
            .is_err()
    );
}

#[test]
fn raw_server_missing_database_path_never_creates_an_empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.sqlite3");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vacancy-search-backend"))
        .env("VACANCY_PORT", "0")
        .env("VACANCY_DB", &missing)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!missing.exists());
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_vacancy-search-backend"))
        .env("VACANCY_PORT", "0")
        .env_remove("VACANCY_DB")
        .output()
        .unwrap();
    assert!(!output.status.success());
}
