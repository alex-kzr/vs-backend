use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tempfile::TempDir;
use tower::ServiceExt;
use vacancy_search_backend::{AppState, open_database, router};

struct TestApp {
    app: Router,
    pool: SqlitePool,
    _dir: TempDir,
}
impl TestApp {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = open_database(&dir.path().join("test.sqlite3"))
            .await
            .unwrap();
        let app = router(
            AppState {
                pool: pool.clone(),
                hosts: vec!["localhost".into()],
                origins: vec!["http://localhost".into()],
                hh: Default::default(),
            },
            None,
        );
        Self {
            app,
            pool,
            _dir: dir,
        }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        input: Value,
        key: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("/api/v1/{path}"))
            .header("host", "localhost")
            .header("content-type", "application/json");
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        let response = self
            .app
            .clone()
            .oneshot(
                request
                    .body(if method == "GET" {
                        Body::empty()
                    } else {
                        Body::from(input.to_string())
                    })
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn post(&self, path: &str, input: Value) -> Value {
        let (status, result) = self
            .call("POST", path, input, Some(&uuid::Uuid::new_v4().to_string()))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{result}");
        result["data"].clone()
    }
    async fn get(&self, path: &str) -> Value {
        let (status, result) = self.call("GET", path, Value::Null, None).await;
        assert_eq!(status, StatusCode::OK, "{result}");
        result
    }
    async fn company(&self, name: &str) -> Value {
        self.post("companies", json!({"name":name})).await
    }
    async fn conversation(&self, company: &Value) -> Value {
        self.post("conversations",json!({"company_id":company["id"],"channel_id":"a0000000-0000-4000-8000-000000000001",
   "participants":[{"participant_key":"self","participant_kind":"user"},{"participant_key":"recruiter","participant_kind":"unknown","display_name":"Recruiter"}]})).await
    }
}
fn input(company: &Value, external: &str) -> Value {
    json!({"source_id":"20000000-0000-4000-8000-000000000001","external_vacancy_id":external,
  "vacancy":{"company_id":company["id"],"title":"Rust Engineer"},
  "observation":{"observation_key":uuid::Uuid::new_v4().to_string(),"observed_at":"2026-10-09T12:00:00Z"}})
}
fn message(conversation: &Value, time: &str) -> Value {
    json!({"sender_participant_id":conversation["participants"][0]["id"],"direction":"outbound",
  "body":"  Recorded text\n","occurred_at":time,"client_message_id":uuid::Uuid::new_v4().to_string()})
}
#[tokio::test]
async fn post_replay_refreshes_record_and_changed_key_payload_conflicts() {
    let t = TestApp::new().await;
    let key = uuid::Uuid::new_v4().to_string();
    let body = json!({"name":"Acme"});
    let (status, a) = t.call("POST", "companies", body.clone(), Some(&key)).await;
    assert_eq!(status, StatusCode::CREATED);
    let path = format!("companies/{}", a["data"]["id"].as_str().unwrap());
    t.call("PATCH", &path, json!({"notes":"updated"}), None)
        .await;
    let (status, b) = t.call("POST", "companies", body, Some(&key)).await;
    assert_eq!(
        (
            status,
            b["deduplicated"].clone(),
            b["data"]["notes"].clone()
        ),
        (StatusCode::OK, json!(true), json!("updated"))
    );
    let (status, error) = t
        .call("POST", "companies", json!({"name":"Changed"}), Some(&key))
        .await;
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("idempotency_conflict"))
    );
    assert_eq!(
        t.get("companies").await["items"].as_array().unwrap().len(),
        1
    );
}
#[tokio::test]
async fn repeated_discovery_keeps_vacancy_status_and_chronological_bounds() {
    let t = TestApp::new().await;
    let company = t.company("Acme").await;
    let body = input(&company, "job-1");
    let first = t.post("vacancy-source-references", body.clone()).await;
    let path = format!("vacancies/{}", first["vacancy_id"].as_str().unwrap());
    t.call("PATCH", &path, json!({"status":"saved"}), None)
        .await;
    let (_, replay) = t
        .call(
            "POST",
            "vacancy-source-references",
            body.clone(),
            Some(&uuid::Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(replay["data"]["discovery_id"], first["discovery_id"]);
    let mut earlier = body;
    earlier["observation"]["observation_key"] = json!(uuid::Uuid::new_v4().to_string());
    earlier["observation"]["observed_at"] = json!("2026-10-08T12:00:00Z");
    let result = t.post("vacancy-source-references", earlier).await;
    assert_eq!(result["vacancy_id"], first["vacancy_id"]);
    let vacancy = t.get(&path).await;
    assert_eq!(
        (
            vacancy["data"]["status"].clone(),
            vacancy["data"]["first_seen_at"].clone(),
            vacancy["data"]["last_seen_at"].clone()
        ),
        (
            json!("saved"),
            json!("2026-10-08T12:00:00.000Z"),
            json!("2026-10-09T12:00:00.000Z")
        )
    );
    assert_eq!(
        t.get(&(path + "/discoveries")).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[tokio::test]
async fn conflicting_locators_rollback_without_partial_history() {
    let t = TestApp::new().await;
    let company = t.company("Acme").await;
    let mut a = input(&company, "one");
    a["external_url"] = json!("https://example.com/one");
    t.post("vacancy-source-references", a).await;
    let mut b = input(&company, "two");
    b["external_url"] = json!("https://example.com/two");
    t.post("vacancy-source-references", b).await;
    let mut bad = input(&company, "one");
    bad["external_url"] = json!("https://example.com/two");
    let (status, error) = t
        .call(
            "POST",
            "vacancy-source-references",
            bad,
            Some(&uuid::Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (StatusCode::CONFLICT, json!("identity_conflict"))
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vacancy_discovery")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}
#[tokio::test]
async fn explicit_link_adds_reference_but_same_title_never_merges() {
    let t = TestApp::new().await;
    let c = t.company("Acme").await;
    let first = t.post("vacancy-source-references", input(&c, "one")).await;
    t.post("vacancy-source-references", input(&c, "two")).await;
    let mut link = input(&c, "three");
    link["target_vacancy_id"] = first["vacancy_id"].clone();
    let result = t.post("vacancy-source-references", link).await;
    assert_eq!(result["vacancy_created"], false);
    let detail = t
        .get(&format!(
            "vacancies/{}",
            first["vacancy_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(detail["data"]["source_count"], 2);
    assert_eq!(
        t.get("vacancies").await["items"].as_array().unwrap().len(),
        2
    );
}
#[tokio::test]
async fn criteria_snapshot_survives_edits_and_unknown_fields_fail() {
    let t = TestApp::new().await;
    let company = t.company("Acme").await;
    let config=t.post("search-configurations",json!({"name":"Remote","criteria_version":1,"criteria":{"keywords":["Rust"],"work_formats":["remote"]}})).await;
    let mut body = input(&company, "one");
    body["observation"]["search_configuration_id"] = config["id"].clone();
    let result = t.post("vacancy-source-references", body).await;
    let path = format!("search-configurations/{}", config["id"].as_str().unwrap());
    t.call(
        "PATCH",
        &path,
        json!({"criteria":{"keywords":["Go"]}}),
        None,
    )
    .await;
    let history = t
        .get(&format!(
            "vacancies/{}/discoveries",
            result["vacancy_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(
        history["items"][0]["criteria_snapshot"]["criteria"]["keywords"],
        json!(["Rust"])
    );
    let (status, _) = t.call("PATCH", &path, json!({"salary":100}), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
#[tokio::test]
async fn cross_company_participant_failure_rolls_back_entire_conversation() {
    let t = TestApp::new().await;
    let a = t.company("A").await;
    let b = t.company("B").await;
    let contact = t
        .post(
            "contacts",
            json!({"company_id":a["id"],"display_name":"Recruiter"}),
        )
        .await;
    let (status,_)=t.call("POST","conversations",json!({"company_id":b["id"],"channel_id":"a0000000-0000-4000-8000-000000000001",
 "participants":[{"participant_key":"self","participant_kind":"user"},{"participant_key":"other","participant_kind":"contact","contact_id":contact["id"]}]}),Some(&uuid::Uuid::new_v4().to_string())).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        t.get("conversations").await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn messages_preserve_body_replay_and_reject_conflicts() {
    let t = TestApp::new().await;
    let c = t.company("Acme").await;
    let conv = t.conversation(&c).await;
    let path = format!("conversations/{}/messages", conv["id"].as_str().unwrap());
    let body = message(&conv, "2026-10-09T12:00:00Z");
    let first = t.post(&path, body.clone()).await;
    let (status, replay) = t
        .call(
            "POST",
            &path,
            body.clone(),
            Some(&uuid::Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(
        (status, replay["data"]["id"].clone()),
        (StatusCode::OK, first["id"].clone())
    );
    assert_eq!(first["body"], "  Recorded text\n");
    let mut bad = body;
    bad["body"] = json!("Edited");
    let (status, _) = t
        .call("POST", &path, bad, Some(&uuid::Uuid::new_v4().to_string()))
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let mut inbound = message(&conv, "2026-10-09T12:00:00Z");
    inbound["direction"] = json!("inbound");
    inbound["sender_participant_id"] = conv["participants"][1]["id"].clone();
    t.post(&path, inbound).await;
    assert_eq!(t.get(&path).await["items"].as_array().unwrap().len(), 2);
}
#[tokio::test]
async fn timeline_cursor_excludes_late_arrivals_until_refresh() {
    let t = TestApp::new().await;
    let c = t.company("Acme").await;
    let conv = t.conversation(&c).await;
    let path = format!("conversations/{}/messages", conv["id"].as_str().unwrap());
    t.post(&path, message(&conv, "2026-10-09T12:00:00Z")).await;
    t.post(&path, message(&conv, "2026-10-09T13:00:00Z")).await;
    let page = t.get(&format!("{path}?limit=1")).await;
    let cursor = page["next_cursor"].as_str().unwrap();
    t.post(&path, message(&conv, "2026-10-09T12:30:00Z")).await;
    let second = t.get(&format!("{path}?limit=1&cursor={cursor}")).await;
    assert_eq!(
        second["items"][0]["occurred_at"],
        "2026-10-09T13:00:00.000Z"
    );
    assert_eq!(t.get(&path).await["items"].as_array().unwrap().len(), 3);
}
#[tokio::test]
async fn concurrent_retries_create_exactly_one_record() {
    let t = TestApp::new().await;
    let key = uuid::Uuid::new_v4().to_string();
    let body = json!({"name":"Race"});
    let (a, b) = tokio::join!(
        t.call("POST", "companies", body.clone(), Some(&key)),
        t.call("POST", "companies", body, Some(&key))
    );
    assert_eq!(a.1["data"]["id"], b.1["data"]["id"]);
    assert_eq!(
        t.get("companies").await["items"].as_array().unwrap().len(),
        1
    );
}
#[tokio::test]
async fn migrations_restart_and_copy_restore_retain_data_and_enforce_integrity() {
    let t = TestApp::new().await;
    t.company("Persistent").await;
    let db = t._dir.path().join("test.sqlite3");
    t.pool.close().await;
    let pool = open_database(&db).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM company")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(integrity, "ok");
    let foreign = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(foreign.is_empty());
    pool.close().await;
    let copy = t._dir.path().join("restored.sqlite3");
    std::fs::copy(db, &copy).unwrap();
    let restored = open_database(&copy).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM company")
        .fetch_one(&restored)
        .await
        .unwrap();
    assert_eq!(count, 1);
    restored.close().await;
}
#[tokio::test]
async fn malformed_json_host_origin_and_validation_errors_have_safe_envelopes() {
    let t = TestApp::new().await;
    for (host, origin, body, status) in [
        ("evil.test", "http://localhost", "{}", StatusCode::FORBIDDEN),
        (
            "localhost",
            "https://evil.test",
            "{}",
            StatusCode::FORBIDDEN,
        ),
        (
            "localhost",
            "http://localhost",
            "{",
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let response = t
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/companies")
                    .header("host", host)
                    .header("origin", origin)
                    .header("content-type", "application/json")
                    .header("idempotency-key", uuid::Uuid::new_v4().to_string())
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        let text = String::from_utf8(
            to_bytes(response.into_body(), 10000)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap();
        assert!(text.contains("request_id") && !text.contains("SELECT"));
    }
    let (status, _) = t
        .call("POST", "companies", json!({"name":"Acme"}), None)
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = t.call("GET", "companies?limit=0", Value::Null, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn fixture_preview_matches_and_changes_no_database_records() {
    let t = TestApp::new().await;
    let config=t.post("search-configurations",json!({"name":"Fixture","criteria_version":1,"criteria":{"keywords":["Rust"],"excluded_keywords":["unpaid"],"work_formats":["remote"]}})).await;
    let preview = t
        .get(&format!(
            "search-configurations/{}/preview",
            config["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(
        (preview["live"].clone(), preview["matched"].clone()),
        (json!(false), json!(1))
    );
    assert_eq!(
        preview["candidates"][0]["external_vacancy_id"],
        "fixture-rust-remote"
    );
    assert!(
        t.get("vacancies").await["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn unicode_literal_search_and_cursor_filter_binding() {
    let t = TestApp::new().await;
    t.company("Компания Пример %").await;
    t.company("Other").await;
    assert_eq!(
        t.get("companies?q=КОМПАНИЯ").await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        t.get("companies?q=%25").await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let first = t.get("companies?limit=1").await;
    let cursor = first["next_cursor"].as_str().unwrap();
    let (status, _) = t
        .call(
            "GET",
            &format!("companies?q=Other&cursor={cursor}"),
            Value::Null,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
#[tokio::test]
async fn role_replacement_and_contact_identity_are_transactional() {
    let t = TestApp::new().await;
    let company = t.company("Acme").await;
    let contact=t.post("contacts",json!({"company_id":company["id"],"display_name":"A","email":"Person@Example.COM","role_ids":["90000000-0000-4000-8000-000000000001"]})).await;
    let path = format!("contacts/{}", contact["id"].as_str().unwrap());
    let (status, _) = t
        .call(
            "PATCH",
            &path,
            json!({"role_ids":["ffffffff-ffff-4fff-8fff-ffffffffffff"]}),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        t.get(&path).await["data"]["role_ids"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (status, _) = t
        .call(
            "POST",
            "contacts",
            json!({"company_id":company["id"],"display_name":"B","email":"Person@example.com"}),
            Some(&uuid::Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    t.call("PATCH", &path, json!({"role_ids":[],"email":null}), None)
        .await;
    assert!(
        t.get(&path).await["data"]["role_ids"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn upgrade_from_core_migration_preserves_ids_and_checksum_drift_is_rejected() {
    use sqlx::sqlite::SqliteConnectOptions;
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("old");
    std::fs::create_dir(&migrations).unwrap();
    std::fs::write(
        migrations.join("0001_core.sql"),
        include_str!("../migrations/0001_core.sql"),
    )
    .unwrap();
    let db = dir.path().join("upgrade.sqlite3");
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true)
            .foreign_keys(true),
    )
    .await
    .unwrap();
    sqlx::migrate::Migrator::new(migrations.as_path())
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO company(id,data) VALUES(?,?)").bind(&id).bind(json!({"id":id,"name":"Тест","website":null,"notes":null,"created_at":"2026-10-09T12:00:00.000Z","updated_at":"2026-10-09T12:00:00.000Z"}).to_string()).execute(&pool).await.unwrap();
    pool.close().await;
    let upgraded = open_database(&db).await.unwrap();
    let normalized: String = sqlx::query_scalar("SELECT normalized_text FROM company WHERE id=?")
        .bind(&id)
        .fetch_one(&upgraded)
        .await
        .unwrap();
    assert_eq!(normalized, "тест");
    sqlx::query("UPDATE _sqlx_migrations SET checksum=X'00' WHERE version=1")
        .execute(&upgraded)
        .await
        .unwrap();
    upgraded.close().await;
    assert!(open_database(&db).await.is_err());
}
#[tokio::test]
async fn failing_migration_is_atomic_and_not_marked_applied() {
    use sqlx::sqlite::SqliteConnectOptions;
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("migrations");
    std::fs::create_dir(&migrations).unwrap();
    std::fs::write(
        migrations.join("0001_broken.sql"),
        "CREATE TABLE should_rollback(id INTEGER); INSERT INTO nonexistent VALUES(1);",
    )
    .unwrap();
    let pool = SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(dir.path().join("broken.sqlite3"))
            .create_if_missing(true),
    )
    .await
    .unwrap();
    assert!(
        sqlx::migrate::Migrator::new(migrations.as_path())
            .await
            .unwrap()
            .run(&pool)
            .await
            .is_err()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE name='should_rollback'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    pool.close().await;
}

#[tokio::test]
async fn concurrent_source_discoveries_keep_one_vacancy_and_two_observations() {
    let t = TestApp::new().await;
    let company = t.company("Source race").await;
    let a = input(&company, "same-job");
    let b = input(&company, "same-job");
    let keya = uuid::Uuid::new_v4().to_string();
    let keyb = uuid::Uuid::new_v4().to_string();
    let (a, b) = tokio::join!(
        t.call("POST", "vacancy-source-references", a, Some(&keya)),
        t.call("POST", "vacancy-source-references", b, Some(&keyb))
    );
    assert_eq!((a.0, b.0), (StatusCode::CREATED, StatusCode::CREATED));
    assert_eq!(a.1["data"]["vacancy_id"], b.1["data"]["vacancy_id"]);
    assert_eq!(
        t.get("vacancies").await["items"].as_array().unwrap().len(),
        1
    );
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM vacancy_discovery")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
}
#[tokio::test]
async fn locked_database_is_retryable_and_has_no_partial_receipt() {
    use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
    let t = TestApp::new().await;
    let mut blocker = SqliteConnection::connect_with(
        &SqliteConnectOptions::new().filename(t._dir.path().join("test.sqlite3")),
    )
    .await
    .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut blocker)
        .await
        .unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    let body = json!({"name":"Retry lock"});
    let (status, error) = t.call("POST", "companies", body.clone(), Some(&key)).await;
    assert_eq!(
        (status, error["error"]["code"].clone()),
        (StatusCode::SERVICE_UNAVAILABLE, json!("database_busy"))
    );
    sqlx::query("ROLLBACK").execute(&mut blocker).await.unwrap();
    let (status, _) = t.call("POST", "companies", body, Some(&key)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        t.get("companies").await["items"].as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn spa_deep_links_serve_index_with_success_without_masking_api_errors() {
    let t = TestApp::new().await;
    let assets = t._dir.path().join("assets");
    std::fs::create_dir(&assets).unwrap();
    std::fs::write(assets.join("index.html"), "<html>synthetic SPA</html>").unwrap();
    std::fs::write(assets.join("app.js"), "console.log('fixture');").unwrap();
    let app = router(
        AppState {
            pool: t.pool.clone(),
            hosts: vec!["localhost".into()],
            origins: vec!["http://localhost".into()],
            hh: Default::default(),
        },
        Some(&assets),
    );
    for path in ["/", "/vacancies", "/contacts/example?panel=history"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("host", "localhost")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/html")
        );
        let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(bytes.as_ref(), b"<html>synthetic SPA</html>");
    }
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/app.js")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
        b"console.log('fixture');"
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/no-such-resource")
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(
        response.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
}
