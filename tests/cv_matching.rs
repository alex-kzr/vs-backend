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
struct App {
    app: Router,
    pool: SqlitePool,
    dir: TempDir,
}
impl App {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = open_database(&dir.path().join("fixture.sqlite3"))
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
        Self { app, pool, dir }
    }
    async fn call(
        &self,
        method: &str,
        path: &str,
        input: Value,
        key: Option<&str>,
    ) -> (StatusCode, Value) {
        let mut r = Request::builder()
            .method(method)
            .uri(format!("/api/v1/{path}"))
            .header("host", "localhost")
            .header("content-type", "application/json");
        if let Some(key) = key {
            r = r.header("Idempotency-Key", key);
        }
        let res = self
            .app
            .clone()
            .oneshot(
                r.body(if method == "GET" {
                    Body::empty()
                } else {
                    Body::from(input.to_string())
                })
                .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = to_bytes(res.into_body(), 4 * 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn post(&self, path: &str, input: Value) -> Value {
        let (s, v) = self
            .call("POST", path, input, Some(&uuid::Uuid::new_v4().to_string()))
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        v["data"].clone()
    }
    async fn get(&self, path: &str) -> Value {
        let (s, v) = self.call("GET", path, Value::Null, None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }
}
fn cv() -> Value {
    json!({"name":"Synthetic Rust CV","hh_url":"https://hh.ru/resume/synthetic_cv_123","content":"Rust. Production APIs and SQL. Remote work.","structured":{"summary":"Backend developer","skills":["Rust","SQL"],"experience":[{"role":"Engineer","details":"Production APIs"}]},"source":{"kind":"manual_text","observed_at":"2026-10-11T06:00:00Z"},"confirmed":true})
}
#[tokio::test]
async fn cv_creates_a_durable_revision_without_starting_matching() {
    let t = App::new().await;
    let created = t.post("cvs", cv()).await;
    assert_eq!(created["current_revision_number"], 1);
    let detail = t
        .get(&format!("cvs/{}", created["id"].as_str().unwrap()))
        .await;
    assert_eq!(detail["data"]["revision"]["content"], cv()["content"]);
    assert_eq!(detail["data"]["revision"]["source"]["kind"], "manual_text");
    assert_eq!(t.get("match-runs").await["items"], json!([]));
    let id = created["id"].as_str().unwrap();
    t.pool.close().await;
    let reopened = open_database(&t.dir.path().join("fixture.sqlite3"))
        .await
        .unwrap();
    let data: String = sqlx::query_scalar("SELECT data FROM cv WHERE id=?")
        .bind(id)
        .fetch_one(&reopened)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&data).unwrap()["current_revision_id"],
        created["current_revision_id"]
    );
}

fn id(v: &Value) -> &str {
    v["id"].as_str().unwrap()
}
fn selection(cv: &Value) -> Value {
    json!({"cv_id":cv["id"],"expected_revision_id":cv["current_revision_id"],"selection":{"statuses":["saved"]}})
}
fn version(cv_meta: &Value) -> Value {
    let mut v = cv();
    v["expected_revision_id"] = cv_meta["current_revision_id"].clone();
    v["source"]["kind"] = json!("manual_edit");
    v["content"] = json!("Rust. SQL. Updated technical CV.");
    v
}
fn assessment() -> Value {
    json!({"summary":"Synthetic transport test, not an LLM analysis.","matches":[{"dimension":"skills","statement":"Rust is mentioned in both inputs.","evidence":[{"source":"cv","path":"/content","quote":"Rust"},{"source":"vacancy","path":"/title","quote":"Rust"}]}],"gaps":[],"questions":[{"dimension":"experience","statement":"Ask about production scale.","evidence":[]}],"unknowns":[{"dimension":"language","statement":"Language is not specified in the vacancy.","evidence":[{"source":"vacancy","path":"/description","quote":null}]}]})
}
fn callback(run: &Value, job: &Value) -> Value {
    json!({"attempt_id":job["attempt_id"],"cv_revision_id":run["cv_revision_id"]})
}
impl App {
    async fn action(&self, path: &str, input: Value) -> Value {
        let (s, v) = self
            .call("POST", path, input, Some(&uuid::Uuid::new_v4().to_string()))
            .await;
        assert_eq!(s, StatusCode::OK, "{path}: {v}");
        v["data"].clone()
    }
    async fn rejects(&self, path: &str, input: Value, status: StatusCode, code: &str) {
        let (s, v) = self
            .call("POST", path, input, Some(&uuid::Uuid::new_v4().to_string()))
            .await;
        assert_eq!(
            (s, v["error"]["code"].as_str().unwrap()),
            (status, code),
            "{path}: {v}"
        );
    }
    async fn vacancy(&self, key: &str, saved: bool) -> Value {
        let company = self
            .post("companies", json!({"name":format!("Synthetic {key}")}))
            .await;
        let created=self.post("vacancy-source-references",json!({"source_id":"20000000-0000-4000-8000-000000000001","external_vacancy_id":key,"vacancy":{"company_id":company["id"],"title":"Rust Engineer"},"observation":{"observation_key":uuid::Uuid::new_v4().to_string(),"observed_at":"2026-10-11T06:00:00Z"}})).await;
        let v = self
            .get(&format!(
                "vacancies/{}",
                created["vacancy_id"].as_str().unwrap()
            ))
            .await["data"]
            .clone();
        if saved {
            self.call(
                "PATCH",
                &format!("vacancies/{}", id(&v)),
                json!({"status":"saved","notes":"Synthetic private note, excluded from matching."}),
                None,
            )
            .await;
        }
        self.get(&format!("vacancies/{}", id(&v))).await["data"].clone()
    }
    async fn setup(&self) -> (Value, Value, Vec<Value>) {
        let cv_meta = self.post("cvs", cv()).await;
        self.vacancy("one", true).await;
        self.vacancy("two", true).await;
        self.vacancy("excluded", false).await;
        let mut input = selection(&cv_meta);
        input["confirmed"] = json!(true);
        let run = self.post("match-runs", input).await;
        let jobs = self.get(&format!("match-runs/{}/jobs", id(&run))).await["items"]
            .as_array()
            .unwrap()
            .clone();
        (cv_meta, run, jobs)
    }
    async fn claim(&self, run: &Value, job: &Value) -> Value {
        let mut body = callback(run, job);
        body["agent"] = json!("synthetic-test");
        self.action(
            &format!("match-runs/{}/jobs/{}/claim", id(run), id(job)),
            body,
        )
        .await
    }
    async fn finish(&self, run: &Value, job: &Value) -> Value {
        let mut body = callback(run, job);
        body["engine"] = json!({"kind":"synthetic","agent":"synthetic-test"});
        body["assessment"] = assessment();
        self.action(
            &format!("match-runs/{}/jobs/{}/complete", id(run), id(job)),
            body,
        )
        .await
    }
}
#[tokio::test]
async fn previews_confirmation_validation_and_multiple_cvs_never_enqueue_work() {
    let t = App::new().await;
    let mut input = cv();
    input.as_object_mut().unwrap().remove("confirmed");
    let preview = t.action("cvs/import-preview", input.clone()).await;
    assert_eq!(preview["content"], cv()["content"]);
    assert_eq!(t.get("cvs").await["items"], json!([]));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM request_receipt")
            .fetch_one(&t.pool)
            .await
            .unwrap(),
        0
    );
    t.rejects(
        "cvs",
        input.clone(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    input["confirmed"] = json!(true);
    input["content"] = json!("  ");
    t.rejects(
        "cvs",
        input,
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    let first = t.post("cvs", cv()).await;
    let mut second = cv();
    second["hh_url"] = Value::Null;
    second["name"] = json!("Alternate CV");
    t.post("cvs", second).await;
    let mut duplicate = cv();
    duplicate["hh_url"] = json!("https://www.hh.ru/resume/synthetic_cv_123/?from=share");
    t.rejects("cvs", duplicate, StatusCode::CONFLICT, "cv_duplicate_url")
        .await;
    assert_eq!(t.get("cvs").await["items"].as_array().unwrap().len(), 2);
    assert_eq!(t.get("match-runs").await["items"], json!([]));
    assert_eq!(
        t.call(
            "PATCH",
            &format!("cvs/{}", id(&first)),
            json!({"content":"mutate"}),
            None
        )
        .await
        .0,
        StatusCode::METHOD_NOT_ALLOWED
    );
}
#[tokio::test]
async fn browser_provenance_is_explicit_and_never_collects_credentials() {
    let t = App::new().await;
    let mut input = cv();
    input["source"]["kind"] = json!("hh_browser");
    t.rejects(
        "cvs",
        input.clone(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    input["source"]["agent"] = json!("explicit-browser-agent");
    input["source"]["url"] = input["hh_url"].clone();
    input["source"]["cookies"] = json!("synthetic");
    t.rejects(
        "cvs",
        input.clone(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    input["source"].as_object_mut().unwrap().remove("cookies");
    let created = t.post("cvs", input).await;
    let detail = t.get(&format!("cvs/{}", id(&created))).await;
    assert_eq!(
        detail["data"]["revision"]["source"]["agent"],
        "explicit-browser-agent"
    );
    assert_eq!(t.get("match-runs").await["items"], json!([]));
}
#[tokio::test]
async fn revisions_are_immutable_optimistic_and_replay_safe() {
    let t = App::new().await;
    let original = t.post("cvs", cv()).await;
    let path = format!("cvs/{}/revisions", id(&original));
    let key = uuid::Uuid::new_v4().to_string();
    let input = version(&original);
    let (s, a) = t.call("POST", &path, input.clone(), Some(&key)).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, b) = t.call("POST", &path, input.clone(), Some(&key)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["data"], b["data"]);
    assert_eq!(b["deduplicated"], true);
    t.rejects(
        &path,
        input.clone(),
        StatusCode::CONFLICT,
        "cv_revision_conflict",
    )
    .await;
    let mut changed = input;
    changed["content"] = json!("Other");
    assert_eq!(
        t.call("POST", &path, changed, Some(&key)).await.1["error"]["code"],
        "idempotency_conflict"
    );
    let old = t
        .get(&format!(
            "{path}/{}",
            original["current_revision_id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(old["data"]["content"], cv()["content"]);
    assert!(
        sqlx::query("UPDATE cv_revision SET data='{}' WHERE id=?")
            .bind(original["current_revision_id"].as_str())
            .execute(&t.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM cv_revision WHERE id=?")
            .bind(original["current_revision_id"].as_str())
            .execute(&t.pool)
            .await
            .is_err()
    );
    let mut other = cv();
    other["hh_url"] = Value::Null;
    let other = t.post("cvs", other).await;
    assert_eq!(
        t.call(
            "GET",
            &format!(
                "cvs/{}/revisions/{}",
                id(&other),
                original["current_revision_id"].as_str().unwrap()
            ),
            Value::Null,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(t.get(&path).await["items"].as_array().unwrap().len(), 2);
    assert_eq!(t.get("match-runs").await["items"], json!([]));
}
#[tokio::test]
async fn explicit_selection_pins_inputs_without_notes_or_status_mutations() {
    let t = App::new().await;
    let cv_meta = t.post("cvs", cv()).await;
    let vacancy = t.vacancy("preview", true).await;
    t.vacancy("not-saved", false).await;
    let before = t.get(&format!("vacancies/{}", id(&vacancy))).await;
    let preview = t.action("match-runs/preview", selection(&cv_meta)).await;
    assert_eq!(preview["count"], 1);
    assert_eq!(t.get("match-runs").await["items"], json!([]));
    t.rejects(
        "match-runs",
        selection(&cv_meta),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    let mut input = selection(&cv_meta);
    input["confirmed"] = json!(true);
    let key = uuid::Uuid::new_v4().to_string();
    let (s, a) = t
        .call("POST", "match-runs", input.clone(), Some(&key))
        .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, b) = t
        .call("POST", "match-runs", input.clone(), Some(&key))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(a["data"]["id"], b["data"]["id"]);
    let repeated = t.post("match-runs", input.clone()).await;
    assert_ne!(a["data"]["id"], repeated["id"]);
    let packet = t
        .get(&format!(
            "match-runs/{}/inputs",
            a["data"]["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(packet["data"]["cv_revision"]["content"], cv()["content"]);
    let snap = &packet["data"]["jobs"][0]["snapshot"];
    assert!(snap.get("notes").is_none());
    assert!(snap.get("status").is_none());
    assert_eq!(t.get(&format!("vacancies/{}", id(&vacancy))).await, before);
    input["selection"]["statuses"] = json!(["hired"]);
    t.rejects(
        "match-runs",
        input.clone(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    input["selection"]["statuses"] = json!(["saved"]);
    input["expected_revision_id"] = json!(uuid::Uuid::new_v4().to_string());
    t.rejects(
        "match-runs",
        input,
        StatusCode::CONFLICT,
        "cv_revision_conflict",
    )
    .await;
}
#[tokio::test]
async fn partial_failure_retry_and_completion_preserve_attempt_history() {
    let t = App::new().await;
    let (_, run, jobs) = t.setup().await;
    t.claim(&run, &jobs[0]).await;
    t.finish(&run, &jobs[0]).await;
    t.claim(&run, &jobs[1]).await;
    let path = format!("match-runs/{}/jobs/{}", id(&run), id(&jobs[1]));
    let mut fail = callback(&run, &jobs[1]);
    fail["error"] = json!({"code":"synthetic_timeout","message":"Synthetic transport failure."});
    t.action(&format!("{path}/fail"), fail).await;
    assert_eq!(
        t.get(&format!("match-runs/{}", id(&run))).await["data"]["state"],
        "partial"
    );
    let retry = t
        .action(&format!("{path}/retry"), callback(&run, &jobs[1]))
        .await;
    assert_eq!(retry["attempt_number"], 2);
    assert_ne!(retry["attempt_id"], jobs[1]["attempt_id"]);
    t.rejects(
        &format!("{path}/claim"),
        {
            let mut v = callback(&run, &jobs[1]);
            v["agent"] = json!("late");
            v
        },
        StatusCode::CONFLICT,
        "matching_input_conflict",
    )
    .await;
    t.claim(&run, &retry).await;
    t.finish(&run, &retry).await;
    let done = t.get(&format!("match-runs/{}", id(&run))).await["data"].clone();
    assert_eq!(done["state"], "completed");
    assert_eq!(done["counts"]["completed"], 2);
    let attempts = t.get(&format!("{path}/attempts")).await;
    assert_eq!(attempts["items"][0]["state"], "completed");
    assert_eq!(attempts["items"][1]["state"], "failed");
    assert!(
        sqlx::query("UPDATE match_attempt SET data='{}' WHERE id=?")
            .bind(jobs[0]["attempt_id"].as_str())
            .execute(&t.pool)
            .await
            .is_err()
    );
    t.rejects(
        &format!("match-runs/{}/cancel", id(&run)),
        json!({}),
        StatusCode::CONFLICT,
        "matching_state_conflict",
    )
    .await;
    t.pool.close().await;
    let reopened = open_database(&t.dir.path().join("fixture.sqlite3"))
        .await
        .unwrap();
    let data: String = sqlx::query_scalar("SELECT data FROM match_job WHERE id=?")
        .bind(id(&retry))
        .fetch_one(&reopened)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&data).unwrap()["assessment"],
        assessment()
    );
}
#[tokio::test]
async fn cancellation_and_interrupted_attempts_reject_late_results() {
    let t = App::new().await;
    let (_, run, jobs) = t.setup().await;
    t.claim(&run, &jobs[0]).await;
    let path = format!("match-runs/{}/jobs/{}", id(&run), id(&jobs[0]));
    let retried = t
        .action(&format!("{path}/retry"), callback(&run, &jobs[0]))
        .await;
    assert_eq!(retried["attempt_number"], 2);
    let attempts = t.get(&format!("{path}/attempts")).await;
    assert_eq!(attempts["items"][1]["error"]["code"], "interrupted");
    t.claim(&run, &retried).await;
    let cancelled = t
        .action(&format!("match-runs/{}/cancel", id(&run)), json!({}))
        .await;
    assert_eq!(cancelled["counts"]["cancelled"], 2);
    assert_eq!(cancelled["state"], "cancelled");
    let mut body = callback(&run, &retried);
    body["engine"] = json!({"kind":"synthetic","agent":"synthetic-test"});
    body["assessment"] = assessment();
    t.rejects(
        &format!("{path}/complete"),
        body,
        StatusCode::CONFLICT,
        "matching_state_conflict",
    )
    .await;
    assert_eq!(
        t.get(&format!("{path}/attempts")).await["items"][0]["state"],
        "cancelled"
    );
}
#[tokio::test]
async fn completion_requires_actual_pinned_quotes_and_professional_dimensions() {
    let t = App::new().await;
    let (_, run, jobs) = t.setup().await;
    t.claim(&run, &jobs[0]).await;
    let path = format!("match-runs/{}/jobs/{}/complete", id(&run), id(&jobs[0]));
    let mut body = callback(&run, &jobs[0]);
    body["engine"] = json!({"kind":"synthetic","agent":"synthetic-test"});
    body["assessment"] = assessment();
    body["assessment"]["matches"][0]["evidence"][0]["quote"] = json!("Invented Kubernetes");
    t.rejects(
        &path,
        body.clone(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    body["assessment"] = assessment();
    body["assessment"]["matches"][0]["dimension"] = json!("age");
    t.rejects(
        &path,
        body.clone(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "validation_failed",
    )
    .await;
    body["assessment"] = assessment();
    body["cv_revision_id"] = json!(uuid::Uuid::new_v4().to_string());
    t.rejects(&path, body, StatusCode::CONFLICT, "matching_input_conflict")
        .await;
    assert_eq!(
        t.get(&format!("match-runs/{}/jobs/{}", id(&run), id(&jobs[0])))
            .await["data"]["state"],
        "running"
    );
    t.finish(&run, &jobs[0]).await;
}
#[tokio::test]
async fn staleness_tracks_relevant_inputs_and_keeps_pinned_history() {
    let t = App::new().await;
    let (cv_meta, run, jobs) = t.setup().await;
    let path = format!("match-runs/{}", id(&run));
    assert_eq!(t.get(&path).await["data"]["stale"], false);
    t.call(
        "PATCH",
        &format!("vacancies/{}", jobs[0]["vacancy_id"].as_str().unwrap()),
        json!({"status":"declined_by_applicant","notes":"Changed private note"}),
        None,
    )
    .await;
    assert_eq!(t.get(&path).await["data"]["stale"], false);
    let newer = t
        .post(
            &format!("cvs/{}/revisions", id(&cv_meta)),
            version(&cv_meta),
        )
        .await;
    assert_eq!(newer["current_revision_number"], 2);
    assert_eq!(t.get(&path).await["data"]["cv_stale"], true);
    assert_eq!(
        t.get(&format!("{path}/inputs")).await["data"]["cv_revision"]["content"],
        cv()["content"]
    );
    sqlx::query(
        "UPDATE vacancy SET data=json_set(data,'$.title','Changed professional title') WHERE id=?",
    )
    .bind(jobs[0]["vacancy_id"].as_str())
    .execute(&t.pool)
    .await
    .unwrap();
    assert_eq!(t.get(&path).await["data"]["stale_vacancies"], 1);
    assert_eq!(
        t.get(&format!("{path}/inputs")).await["data"]["jobs"][0]["snapshot"]["title"],
        "Rust Engineer"
    );
    assert_eq!(
        t.get("match-runs").await["items"].as_array().unwrap().len(),
        1
    );
}

async fn legacy_rows(pool: &SqlitePool) -> std::collections::BTreeMap<String, Vec<String>> {
    use sqlx::Row;
    let names:Vec<String>=sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name!='_sqlx_migrations' ORDER BY name").fetch_all(pool).await.unwrap();
    let mut rows = std::collections::BTreeMap::new();
    for name in names {
        let columns = sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA table_info(\"{name}\")"
        )))
        .fetch_all(pool)
        .await
        .unwrap();
        let expr = columns
            .iter()
            .map(|r| format!("quote(\"{}\")", r.get::<String, _>("name")))
            .collect::<Vec<_>>()
            .join("||'|'||");
        let values: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT {expr} FROM \"{name}\" ORDER BY 1"
        )))
        .fetch_all(pool)
        .await
        .unwrap();
        rows.insert(name, values);
    }
    rows
}
#[tokio::test]
async fn upgrade_from_version_eight_preserves_every_legacy_row_and_migration_checksum() {
    use sqlx::sqlite::SqliteConnectOptions;
    let dir = tempfile::tempdir().unwrap();
    let migrations = dir.path().join("legacy-migrations");
    std::fs::create_dir(&migrations).unwrap();
    for file in
        std::fs::read_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
            .unwrap()
    {
        let file = file.unwrap();
        let name = file.file_name();
        let name = name.to_str().unwrap();
        let version = name.split('_').next().unwrap().parse::<i64>().unwrap();
        if version <= 8 {
            std::fs::copy(file.path(), migrations.join(name)).unwrap();
        }
    }
    let db = dir.path().join("fixture.sqlite3");
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
    let t = App {
        app: router(
            AppState {
                pool: pool.clone(),
                hosts: vec!["localhost".into()],
                origins: vec!["http://localhost".into()],
                hh: Default::default(),
            },
            None,
        ),
        pool,
        dir,
    };
    let vacancy = t.vacancy("legacy", true).await;
    t.post(
        "search-configurations",
        json!({"name":"Legacy search","criteria_version":1,"criteria":{"queries":["Rust"]}}),
    )
    .await;
    let before = legacy_rows(&t.pool).await;
    let checksums: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version,checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&t.pool)
            .await
            .unwrap();
    t.pool.close().await;
    let upgraded = open_database(&db).await.unwrap();
    let after = legacy_rows(&upgraded).await;
    for (name, rows) in before {
        assert_eq!(after.get(&name), Some(&rows), "legacy table {name} changed");
    }
    let after_checksums: Vec<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT version,checksum FROM _sqlx_migrations WHERE version<=8 ORDER BY version",
    )
    .fetch_all(&upgraded)
    .await
    .unwrap();
    assert_eq!(checksums, after_checksums);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&upgraded)
            .await
            .unwrap(),
        10
    );
    assert!(after["cv"].is_empty());
    assert!(after["match_run"].is_empty());
    let raw: String = sqlx::query_scalar("SELECT data FROM vacancy WHERE id=?")
        .bind(id(&vacancy))
        .fetch_one(&upgraded)
        .await
        .unwrap();
    let restored: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(restored["status"], "saved");
    assert_eq!(
        restored["notes"],
        "Synthetic private note, excluded from matching."
    );
}
#[tokio::test]
async fn concurrent_cv_revision_retries_commit_once_and_late_edits_conflict() {
    let t = App::new().await;
    let parent = t.post("cvs", cv()).await;
    let key = uuid::Uuid::new_v4().to_string();
    let path = format!("cvs/{}/revisions", id(&parent));
    let (a, b) = tokio::join!(
        t.call("POST", &path, version(&parent), Some(&key)),
        t.call("POST", &path, version(&parent), Some(&key))
    );
    assert_eq!((a.0, b.0), (StatusCode::CREATED, StatusCode::OK));
    assert_eq!(a.1["data"], b.1["data"]);
    assert_eq!(t.get(&path).await["items"].as_array().unwrap().len(), 2);
    t.rejects(
        &path,
        version(&parent),
        StatusCode::CONFLICT,
        "cv_revision_conflict",
    )
    .await;
}

#[tokio::test]
async fn revision_history_is_bounded_and_cursor_excludes_later_versions() {
    let t = App::new().await;
    let first = t.post("cvs", cv()).await;
    let path = format!("cvs/{}/revisions", id(&first));
    let second = t.post(&path, version(&first)).await;
    let page = t.get(&format!("{path}?limit=1")).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 1);
    assert!(page["items"][0].get("content").is_none());
    t.post(&path, version(&second)).await;
    let next = t
        .get(&format!(
            "{path}?limit=1&cursor={}",
            page["next_cursor"].as_str().unwrap()
        ))
        .await;
    assert_eq!(next["items"][0]["revision_number"], 1);
    assert!(next["next_cursor"].is_null());
    assert_eq!(t.get(&path).await["items"].as_array().unwrap().len(), 3);
    assert_eq!(
        t.call("GET", &format!("{path}?limit=101"), Value::Null, None)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn result_provenance_must_match_the_claimed_agent() {
    let t = App::new().await;
    let (_, run, jobs) = t.setup().await;
    t.claim(&run, &jobs[0]).await;
    let mut body = callback(&run, &jobs[0]);
    body["assessment"] = assessment();
    body["engine"] = json!({"kind":"synthetic","agent":"different-agent"});
    t.rejects(
        &format!("match-runs/{}/jobs/{}/complete", id(&run), id(&jobs[0])),
        body,
        StatusCode::CONFLICT,
        "matching_agent_conflict",
    )
    .await;
    t.finish(&run, &jobs[0]).await;
}
