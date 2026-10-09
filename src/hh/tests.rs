use super::*;
use crate::{AppState, open_database, router};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::Request,
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};
use tempfile::TempDir;
use tower::ServiceExt;

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: String,
    delay: Duration,
    retry: Option<String>,
}
impl Reply {
    fn json(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            body: body.to_string(),
            delay: Duration::ZERO,
            retry: None,
        }
    }
    fn error(status: StatusCode) -> Self {
        Self {
            status,
            body: "upstream-private-debug-must-not-leak".into(),
            delay: Duration::ZERO,
            retry: None,
        }
    }
}
#[derive(Clone)]
struct MockState {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    seen: Arc<Mutex<Vec<(String, String, bool)>>>,
    calls: Arc<AtomicUsize>,
}
struct Mock {
    state: MockState,
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn upstream(State(state): State<MockState>, request: Request<Body>) -> Response {
    state.calls.fetch_add(1, Ordering::SeqCst);
    state.seen.lock().unwrap().push((
        request.uri().to_string(),
        request
            .headers()
            .get("hh-user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        request
            .headers()
            .get("authorization")
            .is_some_and(|x| x == "Bearer synthetic-test-token"),
    ));
    let reply = state
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .unwrap_or_else(|| Reply::error(StatusCode::INTERNAL_SERVER_ERROR));
    tokio::time::sleep(reply.delay).await;
    let mut response = (reply.status, reply.body).into_response();
    if let Some(retry) = reply.retry {
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_str(&retry).unwrap());
    }
    response
}
impl Mock {
    async fn new(replies: Vec<Reply>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let state = MockState {
            replies: Arc::new(Mutex::new(replies.into())),
            seen: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/vacancies", get(upstream))
            .route("/suggests/areas", get(upstream))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { state, url, task }
    }
    fn client(&self) -> HhClient {
        HhClient::ready(
            &self.url,
            "VacancySearch-Test/1.0 (synthetic@example.invalid)",
            Some("synthetic-test-token"),
            Duration::from_secs(2),
            Duration::ZERO,
            false,
        )
        .unwrap()
    }
    async fn started(&self) {
        for _ in 0..100 {
            if self.state.calls.load(Ordering::SeqCst) > 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("owned mock was not called");
    }
}
fn item(id: &str) -> Value {
    json!({"id":id,"name":"Synthetic QA vacancy","alternate_url":"https://hh.ru/vacancy/WRONG?token=must-not-import",
        "employer":{"id":"990001","name":"Synthetic employer"},"area":{"id":"991","name":"Synthetic region"},
        "work_format":[{"id":"REMOTE"}],"employment_form":{"id":"FULL"},
        "salary_range":{"from":120000,"to":150000,"currency":"RUR","gross":true,"mode":{"id":"MONTH","name":"Month"},"frequency":null},
        "snippet":{"requirement":"<highlighttext>QA</highlighttext> &amp; Rust","responsibility":"<script>untrusted()</script> testing"},
        "published_at":"2026-10-09T10:00:00+0300"})
}
fn page(number: u64) -> Value {
    json!({"page":number,"per_page":2,"pages":3,"found":6,"items":[item("70001"),item("70002")]})
}
fn configuration() -> Value {
    json!({"id":Uuid::new_v4().to_string(),"criteria":{"hh":{"text":"Synthetic QA & test","areas":[{"id":"991","name":"Synthetic region"}],"per_page":2,"work_formats":["REMOTE"],"experience":["noExperience"],"employment_forms":["FULL"]}}})
}

struct Local {
    app: Router,
    pool: sqlx::SqlitePool,
    _dir: TempDir,
}
impl Local {
    async fn new(hh: HhClient) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = open_database(&dir.path().join("hh.sqlite3")).await.unwrap();
        let app = router(
            AppState {
                pool: pool.clone(),
                hosts: vec!["localhost".into()],
                origins: vec!["http://localhost".into()],
                hh,
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
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("/api/v1/{path}"))
            .header("host", "localhost")
            .header("content-type", "application/json");
        if let Some(key) = key {
            builder = builder.header("idempotency-key", key);
        }
        let response = self
            .app
            .clone()
            .oneshot(
                builder
                    .body(if method == "GET" || method == "DELETE" {
                        Body::empty()
                    } else {
                        Body::from(input.to_string())
                    })
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), 2 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }
    async fn post(&self, path: &str, input: Value) -> Value {
        let (status, body) = self
            .call("POST", path, input, Some(&Uuid::new_v4().to_string()))
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body["data"].clone()
    }
}

#[test]
fn mapping_keeps_ids_safe_urls_salary_units_and_snippet_provenance() {
    let cfg = configuration();
    let (hh, _) = query::parameters(&cfg, 0).unwrap();
    let mut response = page(0);
    response["items"][1] = response["items"][0].clone();
    let preview = mapping::preview(response, &cfg, &hh, 0, "2026-10-09T12:00:00.000Z").unwrap();
    assert_eq!(preview["matched"], 1);
    assert_eq!(preview["skipped"], 1);
    let row = &preview["candidates"][0];
    assert_eq!(row["external_url"], "https://hh.ru/vacancy/70001");
    assert_eq!(row["vacancy"]["salary_min"], 12_000_000);
    assert_eq!(row["vacancy"]["salary_currency"], "RUB");
    assert_eq!(row["vacancy"]["work_format"], "remote");
    assert_eq!(
        row["source_payload"]["published_at"],
        "2026-10-09T07:00:00.000Z"
    );
    assert_eq!(row["source_payload"]["description_kind"], "search_snippet");
    assert!(
        !row["vacancy"]["description"]
            .as_str()
            .unwrap()
            .contains("<")
    );
    source_payload(&row["source_payload"]).unwrap();
}
#[test]
fn criteria_and_page_bounds_reject_ambiguous_or_unbounded_requests() {
    for value in [
        json!({"per_page":51}),
        json!({"per_page":"2"}),
        json!({"period_days":31}),
        json!({"work_formats":["not-a-format"]}),
        json!({"areas":[{"id":"text","name":"Bad"}]}),
    ] {
        assert!(query::normalize(&value).is_err());
    }
    let cfg = configuration();
    assert!(query::parameters(&cfg, 99).is_ok());
    assert!(query::parameters(&cfg, 100).is_err());
    assert!(query::preview_query("adapter=hh&adapter=fixture-v1").is_err());
    assert!(query::preview_query("adapter=hh&page=-1").is_err());
    assert!(query::parameters(&json!({"criteria":{"hh":{}}}), 0).is_err());
}
#[tokio::test]
async fn official_query_headers_pagination_and_region_suggestions_are_sent_once() {
    let mock = Mock::new(vec![
        Reply::json(page(0)),
        Reply::json(page(1)),
        Reply::json(json!({"items":[{"id":"991","text":"Synthetic region"}]})),
    ])
    .await;
    let hh = mock.client();
    let cfg = configuration();
    let first = hh.preview(&cfg, 0, Uuid::new_v4()).await.unwrap();
    let second = hh.preview(&cfg, 1, Uuid::new_v4()).await.unwrap();
    assert_eq!(first["next_page"], 1);
    assert_eq!(second["page"], 1);
    let areas = hh.areas("text=Synthetic").await.unwrap();
    assert_eq!(areas["items"][0]["name"], "Synthetic region");
    let seen = mock.state.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(seen[0].2);
    assert!(seen[0].1.contains("VacancySearch-Test/"));
    let args: HashMap<_, _> =
        url::form_urlencoded::parse(seen[0].0.split_once('?').unwrap().1.as_bytes()).collect();
    assert_eq!(args["text"], "Synthetic QA & test");
    assert_eq!(args["area"], "991");
    assert_eq!(args["work_format"], "REMOTE");
    assert_eq!(args["employment_form"], "FULL");
    assert_eq!(args["no_magic"], "true");
}
#[tokio::test]
async fn provider_errors_do_not_echo_upstream_or_follow_redirects() {
    for (status, code) in [
        (StatusCode::BAD_REQUEST, "hh_invalid_criteria"),
        (StatusCode::FORBIDDEN, "hh_access_denied"),
        (StatusCode::UNAUTHORIZED, "hh_access_denied"),
        (StatusCode::INTERNAL_SERVER_ERROR, "hh_unavailable"),
        (StatusCode::FOUND, "hh_unavailable"),
    ] {
        let mock = Mock::new(vec![Reply::error(status)]).await;
        let error = mock
            .client()
            .preview(&configuration(), 0, Uuid::new_v4())
            .await
            .unwrap_err();
        assert_eq!(error.code, code);
        assert!(!error.message.contains("upstream-private"));
        assert_eq!(mock.state.calls.load(Ordering::SeqCst), 1);
    }
}
#[tokio::test]
async fn rate_limit_enforces_retry_after_and_never_retries_automatically() {
    let mut reply = Reply::error(StatusCode::TOO_MANY_REQUESTS);
    reply.retry = Some("120".into());
    let mock = Mock::new(vec![reply, Reply::json(page(0))]).await;
    let hh = mock.client();
    let error = hh
        .preview(&configuration(), 0, Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(error.retry_after, Some(120));
    let retry = hh
        .preview(&configuration(), 0, Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(retry.code, "hh_rate_limited");
    assert_eq!(mock.state.calls.load(Ordering::SeqCst), 1);
    let response = error.into_response();
    assert_eq!(response.headers()["retry-after"], "120");
}
#[tokio::test]
async fn malformed_oversized_and_incompatible_pages_fail_before_import() {
    let mut malformed = Reply::json(page(0));
    malformed.body = "not-json".into();
    let mut oversized = Reply::json(page(0));
    oversized.body = "x".repeat(MAX_BODY + 1);
    let mut wrong = page(0);
    wrong["page"] = json!(1);
    for (reply, code) in [
        (malformed, "hh_invalid_response"),
        (oversized, "hh_response_too_large"),
        (Reply::json(wrong), "hh_invalid_response"),
    ] {
        let mock = Mock::new(vec![reply]).await;
        assert_eq!(
            mock.client()
                .preview(&configuration(), 0, Uuid::new_v4())
                .await
                .unwrap_err()
                .code,
            code
        );
    }
}
#[tokio::test]
async fn network_timeout_cancellation_and_busy_requests_are_bounded() {
    let mut slow = Reply::json(page(0));
    slow.delay = Duration::from_secs(1);
    let mock = Mock::new(vec![slow.clone(), Reply::json(page(0))]).await;
    let timeout = HhClient::ready(
        &mock.url,
        "Test/1 (synthetic@example.invalid)",
        Some("synthetic-test-token"),
        Duration::from_millis(50),
        Duration::ZERO,
        false,
    )
    .unwrap();
    assert_eq!(
        timeout
            .preview(&configuration(), 0, Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        "hh_timeout"
    );
    let hh = mock.client();
    let id = Uuid::new_v4();
    hh.cancel(&id.to_string()).unwrap();
    assert_eq!(
        hh.preview(&configuration(), 0, id).await.unwrap_err().code,
        "hh_cancelled"
    );
    let mock = Mock::new(vec![slow, Reply::json(page(0))]).await;
    let hh = mock.client();
    let id = Uuid::new_v4();
    let active = hh.clone();
    let task = tokio::spawn(async move { active.preview(&configuration(), 0, id).await });
    mock.started().await;
    assert_eq!(
        hh.preview(&configuration(), 0, Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        "hh_busy"
    );
    hh.cancel(&id.to_string()).unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(200), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code,
        "hh_cancelled"
    );
    assert!(
        hh.preview(&configuration(), 0, Uuid::new_v4())
            .await
            .is_ok()
    );
}
#[tokio::test]
async fn preview_releases_database_and_cancel_endpoint_stops_upstream() {
    let mut slow = Reply::json(page(0));
    slow.delay = Duration::from_secs(1);
    let mock = Mock::new(vec![slow]).await;
    let local = Local::new(mock.client()).await;
    let cfg=local.post("search-configurations",json!({"name":"Synthetic HH cancellation","criteria_version":1,"criteria":configuration()["criteria"]})).await;
    let id = Uuid::new_v4();
    let app = local.app.clone();
    let path = format!(
        "/api/v1/search-configurations/{}/preview?adapter=hh&request_id={id}",
        cfg["id"].as_str().unwrap()
    );
    let request = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .uri(path)
                .header("host", "localhost")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    });
    mock.started().await;
    let company = tokio::time::timeout(
        Duration::from_millis(200),
        local.post("companies", json!({"name":"Unblocked during HH network"})),
    )
    .await
    .unwrap();
    assert!(!company["id"].is_null());
    let (status, _) = local
        .call("DELETE", &format!("hh/requests/{id}"), Value::Null, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let response = tokio::time::timeout(Duration::from_millis(200), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM vacancy")
            .fetch_one(&local.pool)
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn preview_selected_import_replay_observation_metadata_and_dedup_are_durable() {
    let mock = Mock::new(vec![Reply::json(page(0)), Reply::json(page(0))]).await;
    let local = Local::new(mock.client()).await;
    let company = local
        .post("companies", json!({"name":"Synthetic HH company"}))
        .await;
    let cfg=local.post("search-configurations",json!({"name":"Synthetic HH criteria","criteria_version":1,"criteria":configuration()["criteria"]})).await;
    let path = format!(
        "search-configurations/{}/preview?adapter=hh",
        cfg["id"].as_str().unwrap()
    );
    let (status, preview) = local.call("GET", &path, Value::Null, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM vacancy")
            .fetch_one(&local.pool)
            .await
            .unwrap(),
        0
    );
    let row = &preview["candidates"][0];
    let mut vacancy = row["vacancy"].clone();
    vacancy["company_id"] = company["id"].clone();
    let mut input = json!({"source_id":"20000000-0000-4000-8000-000000000003","external_vacancy_id":row["external_vacancy_id"],"external_url":row["external_url"],"vacancy":vacancy,
        "observation":{"observation_key":Uuid::new_v4().to_string(),"observed_at":row["source_payload"]["fetched_at"],"search_configuration_id":cfg["id"],"source_payload":row["source_payload"]}});
    let key = Uuid::new_v4().to_string();
    let (status, first) = local
        .call(
            "POST",
            "vacancy-source-references",
            input.clone(),
            Some(&key),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (_, retry) = local
        .call(
            "POST",
            "vacancy-source-references",
            input.clone(),
            Some(&key),
        )
        .await;
    assert_eq!(retry["data"]["vacancy_id"], first["data"]["vacancy_id"]);
    input["observation"]["source_payload"]["employer_name"] = json!("Changed");
    let (status, _) = local
        .call(
            "POST",
            "vacancy-source-references",
            input.clone(),
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    input["observation"]["source_payload"]["employer_name"] =
        row["source_payload"]["employer_name"].clone();
    input["observation"]["observation_key"] = json!(Uuid::new_v4().to_string());
    let (_, second) = local
        .call(
            "POST",
            "vacancy-source-references",
            input.clone(),
            Some(&Uuid::new_v4().to_string()),
        )
        .await;
    assert_eq!(second["data"]["vacancy_id"], first["data"]["vacancy_id"]);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM vacancy")
            .fetch_one(&local.pool)
            .await
            .unwrap(),
        1
    );
    let (status, history) = local
        .call(
            "GET",
            &format!(
                "vacancies/{}/discoveries",
                first["data"]["vacancy_id"].as_str().unwrap()
            ),
            Value::Null,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(history["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        history["items"][0]["source_payload"]["salary"]["gross"],
        true
    );
    assert_eq!(
        history["items"][0]["criteria_snapshot"]["criteria"]["hh"]["text"],
        "Synthetic QA & test"
    );
}
#[tokio::test]
async fn missing_access_is_actionable_and_never_contacts_a_provider() {
    let local = Local::new(HhClient::default()).await;
    let cfg=local.post("search-configurations",json!({"name":"Blocked HH","criteria_version":1,"criteria":configuration()["criteria"]})).await;
    let (_, status) = local.call("GET", "hh/status", Value::Null, None).await;
    assert_eq!(status["configured"], false);
    assert!(!status.to_string().contains("synthetic-test-token"));
    let (code, error) = local
        .call(
            "GET",
            &format!(
                "search-configurations/{}/preview?adapter=hh",
                cfg["id"].as_str().unwrap()
            ),
            Value::Null,
            None,
        )
        .await;
    assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error"]["code"], "hh_setup_required");
}

#[tokio::test]
async fn anonymous_requests_need_identification_but_no_artificial_bearer_token() {
    let mock = Mock::new(vec![Reply::json(page(0))]).await;
    let client = HhClient::ready(
        &mock.url,
        "RegisteredApp-Test/1.0 (synthetic@example.invalid)",
        None,
        Duration::from_secs(2),
        Duration::ZERO,
        false,
    )
    .unwrap();
    assert!(
        client
            .preview(&configuration(), 0, Uuid::new_v4())
            .await
            .is_ok()
    );
    let seen = mock.state.seen.lock().unwrap();
    assert!(!seen[0].2);
    assert!(seen[0].1.contains("RegisteredApp-Test"));
}
#[tokio::test]
async fn local_pacing_and_http_date_retry_after_are_respected() {
    let mock = Mock::new(vec![Reply::json(page(0)), Reply::json(page(0))]).await;
    let client = HhClient::ready(
        &mock.url,
        "RegisteredApp-Test/1.0 (synthetic@example.invalid)",
        None,
        Duration::from_secs(2),
        Duration::from_secs(1),
        false,
    )
    .unwrap();
    client
        .preview(&configuration(), 0, Uuid::new_v4())
        .await
        .unwrap();
    assert_eq!(
        client
            .preview(&configuration(), 0, Uuid::new_v4())
            .await
            .unwrap_err()
            .code,
        "hh_rate_limited"
    );
    assert_eq!(mock.state.calls.load(Ordering::SeqCst), 1);
    let future = (chrono::Utc::now() + chrono::Duration::seconds(90)).to_rfc2822();
    assert!((89..=90).contains(&retry_after(Some(&future))));
    assert_eq!(retry_after(Some("invalid")), 60);
}
