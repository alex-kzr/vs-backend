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
    _dir: TempDir,
}
impl App {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let pool = open_database(&dir.path().join("synthetic.sqlite3"))
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
        let mut r = Request::builder()
            .method(method)
            .uri(format!("/api/v1/{path}"))
            .header("host", "localhost")
            .header("content-type", "application/json");
        if let Some(k) = key {
            r = r.header("Idempotency-Key", k);
        }
        let response = self
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
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    async fn post(&self, path: &str, input: Value) -> Value {
        let (s, v) = self
            .call("POST", path, input, Some(&uuid::Uuid::new_v4().to_string()))
            .await;
        assert!(s.is_success(), "{s}: {v}");
        v["data"].clone()
    }
    async fn get(&self, path: &str) -> Value {
        let (s, v) = self.call("GET", path, Value::Null, None).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }
    async fn cv(&self, name: &str) -> Value {
        self.post("cvs", draft(name)).await
    }
    async fn run(&self, cv: &Value, reference: Option<&Value>) -> Value {
        self.post("cv-review-runs",json!({"cv_id":cv["id"],"expected_revision_id":cv["current_revision_id"],"reference_cv_id":reference.map(|r|r["id"].clone()),"expected_reference_revision_id":reference.map(|r|r["current_revision_id"].clone()),"confirmed":true})).await
    }
    async fn claim(&self, run: &Value) -> Value {
        self.post(
            &format!("cv-review-runs/{}/claim", run["id"].as_str().unwrap()),
            json!({"expected_version":run["version"],"agent":"synthetic-test"}),
        )
        .await
    }
    async fn complete(&self, run: &Value, findings: Value, resolutions: Value) -> Value {
        self.post(&format!("cv-review-runs/{}/complete",run["id"].as_str().unwrap()),json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":findings,"resolutions":resolutions})).await
    }
}
fn draft(name: &str) -> Value {
    json!({"name":name,"content":"Synthetic CV. Rust APIs. SQL.","structured":{},"source":{"kind":"manual_text","observed_at":"2026-10-14T12:00:00Z"},"confirmed":true})
}
fn finding(cv: &Value) -> Value {
    json!({"issue_key":"summary-specificity","field":"summary","mismatch":"Описание задач недостаточно конкретно.","suggested_edit":"Уточнить, какие API разрабатывались, если это соответствует опыту.","rationale":"В тексте указаны API, но нет описания задач.","evidence":[{"revision_id":cv["current_revision_id"],"quote":"Rust APIs"}]})
}
fn resolution(cv: &Value) -> Value {
    json!({"issue_key":"summary-specificity","rationale":"Повторно проверено описание задач.","evidence":[{"revision_id":cv["current_revision_id"],"quote":"Rust APIs"}]})
}
#[tokio::test]
async fn explicit_run_pins_inputs_and_preserves_delivery_and_decisions() {
    let t = App::new().await;
    let cv = t.cv("Synthetic target").await;
    let created = t.run(&cv, None).await;
    assert_eq!(created["state"], "queued");
    assert_eq!(
        t.get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"],
        json!([])
    );
    let packet = t
        .get(&format!(
            "cv-review-runs/{}/inputs",
            created["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(
        packet["data"]["target_revision"]["content"],
        draft("x")["content"]
    );
    let run = t.claim(&created).await;
    let key = uuid::Uuid::new_v4().to_string();
    let body = json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[finding(&cv)],"resolutions":[]});
    let path = format!("cv-review-runs/{}/complete", run["id"].as_str().unwrap());
    let (s, first) = t.call("POST", &path, body.clone(), Some(&key)).await;
    assert_eq!(s, StatusCode::OK, "{first}");
    let (s, replay) = t.call("POST", &path, body.clone(), Some(&key)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(replay["deduplicated"], true);
    assert_eq!(first["data"], replay["data"]);
    let mut changed = body;
    changed["findings"] = json!([]);
    assert_eq!(
        t.call("POST", &path, changed, Some(&key)).await.0,
        StatusCode::CONFLICT
    );
    let mut rec = t
        .get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]
        .clone();
    rec = t
        .post(
            &format!(
                "cv-recommendations/{}/decision",
                rec["id"].as_str().unwrap()
            ),
            json!({"expected_version":rec["version"],"state":"rejected","reason":"Не подходит."}),
        )
        .await;
    let second = t.run(&cv, None).await;
    let second = t.claim(&second).await;
    t.complete(&second, json!([finding(&cv)]), json!([])).await;
    let current = t
        .get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]
        .clone();
    assert_eq!(current["id"], rec["id"]);
    assert_eq!(current["state"], "rejected");
    assert_eq!(current["decision_reason"], "Не подходит.");
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM cv_recommendation")
            .fetch_one(&t.pool)
            .await
            .unwrap(),
        1
    );
    let history = t
        .get(&format!(
            "cv-recommendations/{}/history",
            rec["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(history["items"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn import_never_resolves_and_stale_result_cannot_apply() {
    let t = App::new().await;
    let cv = t.cv("Synthetic review versions").await;
    let run = t.run(&cv, None).await;
    let run = t.claim(&run).await;
    t.complete(&run, json!([finding(&cv)]), json!([])).await;
    let pending = t.run(&cv, None).await;
    let old = t.claim(&pending).await;
    let mut updated = draft("Synthetic updated CV");
    updated["content"] = json!("Synthetic CV. Rust APIs with explicit tasks. SQL.");
    updated["expected_revision_id"] = cv["current_revision_id"].clone();
    let next = t
        .post(
            &format!("cvs/{}/revisions", cv["id"].as_str().unwrap()),
            updated,
        )
        .await;
    let recs = t
        .get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(recs["items"][0]["state"], "active");
    assert_eq!(recs["items"][0]["stale"], true);
    let packet = t
        .get(&format!(
            "cv-review-runs/{}/inputs",
            old["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(
        packet["data"]["target_revision"]["id"],
        cv["current_revision_id"]
    );
    let completed = t.complete(&old, json!([]), json!([resolution(&cv)])).await;
    assert_eq!(completed["applied"], false);
    assert_eq!(
        t.get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]["state"],
        "active"
    );
    let fresh = t.run(&next, None).await;
    let fresh = t.claim(&fresh).await;
    // Omitting an issue does not resolve it, even after a fresh explicit review.
    t.complete(&fresh, json!([]), json!([])).await;
    assert_eq!(
        t.get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]["state"],
        "active"
    );
    let checked = t.run(&next, None).await;
    let checked = t.claim(&checked).await;
    t.complete(&checked, json!([]), json!([resolution(&next)]))
        .await;
    let current = t
        .get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]
        .clone();
    assert_eq!(current["state"], "resolved");
    assert_eq!(current["cv_revision_id"], next["current_revision_id"]);
    assert_eq!(current["stale"], false);
    let again = t.run(&next, None).await;
    let again = t.claim(&again).await;
    t.complete(&again, json!([finding(&next)]), json!([])).await;
    assert_eq!(
        t.get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]["state"],
        "active"
    );
}
#[tokio::test]
async fn comparison_direction_and_reference_revision_are_preserved() {
    let t = App::new().await;
    let a = t.cv("Synthetic A").await;
    let b = t.cv("Synthetic B").await;
    for (target, reference) in [(&a, &b), (&b, &a)] {
        let run = t.run(target, Some(reference)).await;
        let run = t.claim(&run).await;
        let mut f = finding(target);
        f["evidence"]
            .as_array_mut()
            .unwrap()
            .push(json!({"revision_id":reference["current_revision_id"],"quote":"Rust APIs"}));
        f["category"] = json!("omitted_supported_experience");
        t.complete(&run, json!([f]), json!([])).await;
    }
    let ar = t
        .get(&format!(
            "cvs/{}/recommendations",
            a["id"].as_str().unwrap()
        ))
        .await;
    let br = t
        .get(&format!(
            "cvs/{}/recommendations",
            b["id"].as_str().unwrap()
        ))
        .await;
    assert_ne!(ar["items"][0]["id"], br["items"][0]["id"]);
    assert_eq!(ar["items"][0]["reference_cv_id"], b["id"]);
    assert_eq!(br["items"][0]["reference_cv_id"], a["id"]);
    let mut updated = draft("Synthetic B next");
    updated["expected_revision_id"] = b["current_revision_id"].clone();
    t.post(
        &format!("cvs/{}/revisions", b["id"].as_str().unwrap()),
        updated,
    )
    .await;
    assert_eq!(
        t.get(&format!(
            "cvs/{}/recommendations",
            a["id"].as_str().unwrap()
        ))
        .await["items"][0]["stale"],
        true
    );
    let(s,v)=t.call("POST","cv-review-runs",json!({"cv_id":a["id"],"expected_revision_id":a["current_revision_id"],"reference_cv_id":a["id"],"expected_reference_revision_id":a["current_revision_id"],"confirmed":true}),Some(&uuid::Uuid::new_v4().to_string())).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
}
#[tokio::test]
async fn failure_retry_cancellation_and_history_survive_restart() {
    let t = App::new().await;
    let cv = t.cv("Synthetic retry").await;
    let created = t.run(&cv, None).await;
    let claimed = t.claim(&created).await;
    let path = format!("cv-review-runs/{}", claimed["id"].as_str().unwrap());
    let failed=t.post(&(path.clone()+"/fail"),json!({"expected_version":claimed["version"],"attempt_id":claimed["attempt_id"],"error":"Synthetic interrupted review"})).await;
    let retry = t
        .post(
            &(path.clone() + "/retry"),
            json!({"expected_version":failed["version"]}),
        )
        .await;
    let next = t.claim(&retry).await;
    assert_ne!(next["attempt_id"], claimed["attempt_id"]);
    let stale = json!({"expected_version":next["version"],"attempt_id":claimed["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[],"resolutions":[]});
    assert_eq!(
        t.call(
            "POST",
            &(path.clone() + "/complete"),
            stale,
            Some(&uuid::Uuid::new_v4().to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let cancelled = t
        .post(
            &(path.clone() + "/cancel"),
            json!({"expected_version":next["version"]}),
        )
        .await;
    let late = json!({"expected_version":cancelled["version"],"attempt_id":next["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[],"resolutions":[]});
    assert_eq!(
        t.call(
            "POST",
            &(path.clone() + "/complete"),
            late,
            Some(&uuid::Uuid::new_v4().to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let page = t.get(&(path.clone() + "/history?limit=2")).await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert!(page["next_cursor"].is_string());
    let second = t
        .get(&(path.clone() + "/history?limit=2&cursor=" + page["next_cursor"].as_str().unwrap()))
        .await;
    assert_ne!(page["items"][0]["id"], second["items"][0]["id"]);
    assert!(
        sqlx::query("UPDATE cv_review_event SET data='{}'")
            .execute(&t.pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM cv_review_run")
            .execute(&t.pool)
            .await
            .is_err()
    );
    t.pool.close().await;
    let reopened = open_database(&t._dir.path().join("synthetic.sqlite3"))
        .await
        .unwrap();
    let saved: String = sqlx::query_scalar("SELECT data FROM cv_review_run WHERE id=?")
        .bind(claimed["id"].as_str())
        .fetch_one(&reopened)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&saved).unwrap()["state"],
        "cancelled"
    );
    reopened.close().await;
}
#[tokio::test]
async fn invalid_provenance_and_quotes_roll_back_and_newer_review_wins() {
    let t = App::new().await;
    let cv = t.cv("Synthetic ordered review").await;
    let older = t.run(&cv, None).await;
    let older = t.claim(&older).await;
    let path = format!("cv-review-runs/{}/complete", older["id"].as_str().unwrap());
    let mut f = finding(&cv);
    f["evidence"][0]["quote"] = json!("Invented unsupported experience");
    let body = json!({"expected_version":older["version"],"attempt_id":older["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[f],"resolutions":[]});
    assert_eq!(
        t.call(
            "POST",
            &path,
            body.clone(),
            Some(&uuid::Uuid::new_v4().to_string())
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let mut provenance = body;
    provenance["findings"] = json!([finding(&cv)]);
    provenance["provenance"]["agent"] = json!("other-agent");
    assert_eq!(
        t.call(
            "POST",
            &path,
            provenance,
            Some(&uuid::Uuid::new_v4().to_string())
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        t.get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"],
        json!([])
    );
    let latest = t.run(&cv, None).await;
    let latest = t.claim(&latest).await;
    t.complete(&latest, json!([finding(&cv)]), json!([])).await;
    let ignored = t.complete(&older, json!([finding(&cv)]), json!([])).await;
    assert_eq!(ignored["applied"], false);
    assert_eq!(ignored["stale"], true);
    let rec = t
        .get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]
        .clone();
    assert_eq!(rec["last_run_id"], latest["id"]);
    let decision_path = format!(
        "cv-recommendations/{}/decision",
        rec["id"].as_str().unwrap()
    );
    let changed=t.post(&decision_path,json!({"expected_version":rec["version"],"state":"rejected","reason":"Synthetic user decision"})).await;
    assert_eq!(
        t.call(
            "POST",
            &decision_path,
            json!({"expected_version":rec["version"],"state":"active"}),
            Some(&uuid::Uuid::new_v4().to_string())
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let restored = t
        .post(
            &decision_path,
            json!({"expected_version":changed["version"],"state":"active"}),
        )
        .await;
    assert_eq!(restored["state"], "active");
    assert!(
        sqlx::query("UPDATE cv_recommendation_event SET data='{}'")
            .execute(&t.pool)
            .await
            .is_err()
    );
}
async fn vacancy(t: &App, title: &str) -> Value {
    let company = t
        .post(
            "companies",
            json!({"name":format!("Synthetic {}",uuid::Uuid::new_v4())}),
        )
        .await;
    let key = uuid::Uuid::new_v4().to_string();
    let reference=t.post("vacancy-source-references",json!({"source_id":"20000000-0000-4000-8000-000000000001","external_vacancy_id":key,"vacancy":{"company_id":company["id"],"title":title,"description":"Required: Kubernetes deployment."},"observation":{"observation_key":key,"observed_at":"2026-10-14T12:00:00Z"}})).await;
    t.get(&format!(
        "vacancies/{}",
        reference["vacancy_id"].as_str().unwrap()
    ))
    .await["data"]
        .clone()
}
#[tokio::test]
async fn vacancy_gaps_require_complete_coverage_and_aggregate_evidence() {
    let t = App::new().await;
    let cv = t.cv("Synthetic vacancy target").await;
    let other = t.cv("Synthetic context").await;
    let a = vacancy(&t, "Synthetic Kubernetes A").await;
    let b = vacancy(&t, "Synthetic Kubernetes B").await;
    let selection = json!({"cv_id":cv["id"],"expected_revision_id":cv["current_revision_id"],"vacancies":[{"id":a["id"],"expected_updated_at":a["updated_at"]},{"id":b["id"],"expected_updated_at":b["updated_at"]}],"context_cvs":[{"id":other["id"],"expected_revision_id":other["current_revision_id"]}],"confirmed":true});
    let run = t.post("cv-review-runs", selection).await;
    let run = t.claim(&run).await;
    let packet = t
        .get(&format!(
            "cv-review-runs/{}/inputs",
            run["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(packet["data"]["vacancies"].as_array().unwrap().len(), 2);
    assert_eq!(
        packet["data"]["context_revisions"][0]["id"],
        other["current_revision_id"]
    );
    let mut f = finding(&cv);
    f["issue_key"] = json!("kubernetes-not-confirmed");
    f["category"] = json!("unconfirmed_requirement");
    f["evidence"].as_array_mut().unwrap().extend([json!({"vacancy_id":a["id"],"vacancy_version":a["updated_at"],"quote":"Kubernetes deployment"}),json!({"vacancy_id":b["id"],"vacancy_version":b["updated_at"],"quote":"Kubernetes deployment"})]);
    let assessments = json!([{"vacancy_id":a["id"],"vacancy_version":a["updated_at"],"relevance":"partial","summary":"Synthetic Kubernetes gap","issue_keys":["kubernetes-not-confirmed"]},{"vacancy_id":b["id"],"vacancy_version":b["updated_at"],"relevance":"partial","summary":"Synthetic Kubernetes gap","issue_keys":["kubernetes-not-confirmed"]}]);
    let path = format!("cv-review-runs/{}/complete", run["id"].as_str().unwrap());
    let body = json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[f],"resolutions":[],"vacancy_assessments":assessments});
    let mut incomplete = body.clone();
    incomplete["vacancy_assessments"]
        .as_array_mut()
        .unwrap()
        .pop();
    assert_eq!(
        t.call(
            "POST",
            &path,
            incomplete,
            Some(&uuid::Uuid::new_v4().to_string())
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    t.post(&path, body).await;
    let rec = t
        .get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]
        .clone();
    assert_eq!(rec["kind"], "vacancy_gap");
    assert_eq!(rec["vacancy_frequency"], 2);
    assert_eq!(rec["vacancy_evidence"].as_array().unwrap().len(), 2);
    let (s, _) = t
        .call(
            "PATCH",
            &format!("vacancies/{}", a["id"].as_str().unwrap()),
            json!({"notes":"Synthetic revision change"}),
            None,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        t.get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]["stale"],
        true
    );
    assert_eq!(
        t.get(&format!(
            "cv-review-runs/{}/inputs",
            run["id"].as_str().unwrap()
        ))
        .await["data"]["vacancies"][0]["snapshot"]["description"],
        "Required: Kubernetes deployment."
    );
    let mut next = draft("Synthetic changed context");
    next["expected_revision_id"] = other["current_revision_id"].clone();
    t.post(
        &format!("cvs/{}/revisions", other["id"].as_str().unwrap()),
        next,
    )
    .await;
    assert!(
        t.get(&format!("cv-review-runs/{}", run["id"].as_str().unwrap()))
            .await["data"]["stale_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("context_changed"))
    );
}

async fn vacancy_run(t: &App, cv: &Value, vacancies: &[Value]) -> Value {
    t.post("cv-review-runs",json!({"cv_id":cv["id"],"expected_revision_id":cv["current_revision_id"],"vacancies":vacancies.iter().map(|v|json!({"id":v["id"],"expected_updated_at":v["updated_at"]})).collect::<Vec<_>>(),"confirmed":true})).await
}
fn vacancy_reports(vacancies: &[Value], keys: Value) -> Value {
    json!(vacancies.iter().map(|v|json!({"vacancy_id":v["id"],"vacancy_version":v["updated_at"],"relevance":"partial","summary":"Synthetic explicit coverage","issue_keys":keys})).collect::<Vec<_>>())
}
fn gap_finding(cv: &Value, vacancies: &[Value], mismatch: &str, multiple: bool) -> Value {
    let mut f = finding(cv);
    f["issue_key"] = json!("kubernetes-gap");
    f["category"] = json!("unconfirmed_requirement");
    f["mismatch"] = json!(mismatch);
    for v in vacancies {
        f["evidence"].as_array_mut().unwrap().push(json!({"vacancy_id":v["id"],"vacancy_version":v["updated_at"],"quote":"Kubernetes deployment"}));
        if multiple {
            f["evidence"].as_array_mut().unwrap().push(
                json!({"vacancy_id":v["id"],"vacancy_version":v["updated_at"],"quote":"Required"}),
            );
        }
    }
    f
}
async fn complete_gap(
    t: &App,
    run: &Value,
    cv: &Value,
    vacancies: &[Value],
    mismatch: &str,
    multiple: bool,
) -> Value {
    let run = t.claim(run).await;
    t.post(&format!("cv-review-runs/{}/complete",run["id"].as_str().unwrap()),json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[gap_finding(cv,vacancies,mismatch,multiple)],"resolutions":[],"vacancy_assessments":vacancy_reports(vacancies,json!(["kubernetes-gap"]))})).await
}
#[tokio::test]
async fn disjoint_late_batches_merge_all_quotes_and_preserve_source_freshness() {
    let t = App::new().await;
    let cv = t.cv("Synthetic batch target").await;
    let a = vacancy(&t, "Synthetic A").await;
    let b = vacancy(&t, "Synthetic B").await;
    let older = vacancy_run(&t, &cv, std::slice::from_ref(&a)).await;
    let newer = vacancy_run(&t, &cv, std::slice::from_ref(&b)).await;
    complete_gap(
        &t,
        &newer,
        &cv,
        std::slice::from_ref(&b),
        "Newer description",
        false,
    )
    .await;
    let late = complete_gap(
        &t,
        &older,
        &cv,
        std::slice::from_ref(&a),
        "Older description",
        true,
    )
    .await;
    assert_eq!(late["applied"], true);
    let path = format!("cvs/{}/recommendations", cv["id"].as_str().unwrap());
    let rec = t.get(&path).await["items"][0].clone();
    assert_eq!(rec["vacancy_frequency"], 2);
    assert_eq!(rec["fresh_vacancy_frequency"], 2);
    assert_eq!(rec["stale"], false);
    assert_eq!(rec["mismatch"], "Newer description");
    assert_eq!(rec["vacancy_evidence"].as_array().unwrap().len(), 3);
    assert!(
        rec["vacancy_evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["run_id"].is_string() && e["cv_revision_id"] == cv["current_revision_id"])
    );
    assert_eq!(rec["evidence"][0]["cv_id"], cv["id"]);
    t.call(
        "PATCH",
        &format!("vacancies/{}", a["id"].as_str().unwrap()),
        json!({"notes":"Synthetic changed A"}),
        None,
    )
    .await;
    let rec = t.get(&path).await["items"][0].clone();
    assert_eq!(rec["stale"], true);
    assert_eq!(rec["fresh_vacancy_frequency"], 1);
    let mut updated = draft("Synthetic new target");
    updated["expected_revision_id"] = cv["current_revision_id"].clone();
    let next = t
        .post(
            &format!("cvs/{}/revisions", cv["id"].as_str().unwrap()),
            updated,
        )
        .await;
    let next_run = vacancy_run(&t, &next, std::slice::from_ref(&b)).await;
    complete_gap(
        &t,
        &next_run,
        &next,
        std::slice::from_ref(&b),
        "Fresh partial target",
        false,
    )
    .await;
    let rec = t.get(&path).await["items"][0].clone();
    assert_eq!(rec["vacancy_frequency"], 2);
    assert_eq!(rec["fresh_vacancy_frequency"], 1);
    assert_eq!(rec["state"], "active");
    assert_eq!(rec["stale"], true);
}
#[tokio::test]
async fn overlapping_newer_batch_does_not_hide_unreviewed_sources_of_older_batch() {
    let t = App::new().await;
    let cv = t.cv("Synthetic overlap").await;
    let a = vacancy(&t, "Synthetic overlap A").await;
    let b = vacancy(&t, "Synthetic overlap B").await;
    let both = vec![a.clone(), b.clone()];
    let older = vacancy_run(&t, &cv, &both).await;
    t.call(
        "PATCH",
        &format!("vacancies/{}", a["id"].as_str().unwrap()),
        json!({"notes":"Synthetic A changed before newer review"}),
        None,
    )
    .await;
    let a_new = t
        .get(&format!("vacancies/{}", a["id"].as_str().unwrap()))
        .await["data"]
        .clone();
    let newer = vacancy_run(&t, &cv, std::slice::from_ref(&a_new)).await;
    let newer = t.claim(&newer).await;
    t.post(&format!("cv-review-runs/{}/complete",newer["id"].as_str().unwrap()),json!({"expected_version":newer["version"],"attempt_id":newer["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[],"resolutions":[],"vacancy_assessments":vacancy_reports(std::slice::from_ref(&a_new),json!([]))})).await;
    let late = complete_gap(&t, &older, &cv, &both, "Synthetic overlap gap", false).await;
    assert_eq!(late["applied"], true);
    assert_eq!(late["superseded_vacancy_ids"], json!([a["id"]]));
    assert_eq!(
        late["result"]["vacancy_assessments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let rec = t
        .get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]
        .clone();
    assert_eq!(rec["vacancy_frequency"], 1);
    assert_eq!(rec["vacancy_evidence"][0]["vacancy_id"], b["id"]);
}
#[tokio::test]
async fn aggregated_gap_over_one_hundred_sources_resolves_in_explicit_batches() {
    let t = App::new().await;
    let cv = t.cv("Synthetic large scope").await;
    let mut first = Vec::new();
    for i in 0..100 {
        first.push(vacancy(&t, &format!("Synthetic large {i}")).await);
    }
    let last = vec![vacancy(&t, "Synthetic large final").await];
    let a = vacancy_run(&t, &cv, &first).await;
    complete_gap(&t, &a, &cv, &first, "Large gap", false).await;
    let b = vacancy_run(&t, &cv, &last).await;
    complete_gap(&t, &b, &cv, &last, "Large gap", false).await;
    let path = format!("cvs/{}/recommendations", cv["id"].as_str().unwrap());
    assert_eq!(t.get(&path).await["items"][0]["vacancy_frequency"], 101);
    for (vacancies, expected) in [(&first, "active"), (&last, "resolved")] {
        let run = vacancy_run(&t, &cv, vacancies).await;
        let run = t.claim(&run).await;
        let evidence = gap_finding(&cv, vacancies, "checked", false)["evidence"].clone();
        t.post(&format!("cv-review-runs/{}/complete",run["id"].as_str().unwrap()),json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[],"resolutions":[{"issue_key":"kubernetes-gap","rationale":"Synthetic explicit source recheck","evidence":evidence,"vacancy_ids":vacancies.iter().map(|v|v["id"].clone()).collect::<Vec<_>>()}],"vacancy_assessments":vacancy_reports(vacancies,json!([]))})).await;
        let rec = t.get(&path).await["items"][0].clone();
        assert_eq!(rec["state"], expected);
        assert_eq!(rec["stale"], false);
    }
    assert_eq!(
        t.get(&path).await["items"][0]["source_resolutions"]
            .as_array()
            .unwrap()
            .len(),
        101
    );
}

#[tokio::test]
async fn one_review_can_resolve_one_source_and_keep_another_for_the_same_issue() {
    let t = App::new().await;
    let cv = t.cv("Synthetic mixed").await;
    let a = vacancy(&t, "Synthetic mixed A").await;
    let b = vacancy(&t, "Synthetic mixed B").await;
    let both = vec![a.clone(), b.clone()];
    let baseline = vacancy_run(&t, &cv, &both).await;
    complete_gap(&t, &baseline, &cv, &both, "Mixed gap", false).await;
    let run = vacancy_run(&t, &cv, &both).await;
    let run = t.claim(&run).await;
    let path = format!("cv-review-runs/{}/complete", run["id"].as_str().unwrap());
    let body = json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[gap_finding(&cv,std::slice::from_ref(&b),"Still required B",false)],"resolutions":[{"issue_key":"kubernetes-gap","rationale":"Explicitly checked A","evidence":gap_finding(&cv,std::slice::from_ref(&a),"checked",false)["evidence"],"vacancy_ids":[a["id"]]}],"vacancy_assessments":[{"vacancy_id":a["id"],"vacancy_version":a["updated_at"],"relevance":"high","summary":"Synthetic checked A","issue_keys":[]},{"vacancy_id":b["id"],"vacancy_version":b["updated_at"],"relevance":"partial","summary":"Synthetic remaining B","issue_keys":["kubernetes-gap"]}]});
    let mut contradictory = body.clone();
    contradictory["resolutions"][0]["vacancy_ids"] = json!([b["id"]]);
    assert_eq!(
        t.call(
            "POST",
            &path,
            contradictory,
            Some(&uuid::Uuid::new_v4().to_string())
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    t.post(&path, body).await;
    let path = format!("cvs/{}/recommendations", cv["id"].as_str().unwrap());
    let rec = t.get(&path).await["items"][0].clone();
    assert_eq!(rec["state"], "active");
    assert_eq!(rec["vacancy_frequency"], 2);
    assert_eq!(rec["mismatch"], "Still required B");
    assert_eq!(rec["source_resolutions"][0]["vacancy_id"], a["id"]);
    assert_eq!(rec["vacancy_evidence"].as_array().unwrap().len(), 2);
    let run = vacancy_run(&t, &cv, std::slice::from_ref(&b)).await;
    let run = t.claim(&run).await;
    t.post(&format!("cv-review-runs/{}/complete",run["id"].as_str().unwrap()),json!({"expected_version":run["version"],"attempt_id":run["attempt_id"],"provenance":{"agent":"synthetic-test","method":"manual-review"},"findings":[],"resolutions":[{"issue_key":"kubernetes-gap","rationale":"Explicitly checked B","evidence":gap_finding(&cv,std::slice::from_ref(&b),"checked",false)["evidence"],"vacancy_ids":[b["id"]]}],"vacancy_assessments":vacancy_reports(std::slice::from_ref(&b),json!([]))})).await;
    assert_eq!(t.get(&path).await["items"][0]["state"], "resolved");
}

#[tokio::test]
async fn vacancy_snapshot_detects_changes_even_with_same_timestamp() {
    let t = App::new().await;
    let cv = t.cv("Synthetic precise snapshot").await;
    let a = vacancy(&t, "Synthetic same time").await;
    let run = vacancy_run(&t, &cv, std::slice::from_ref(&a)).await;
    complete_gap(
        &t,
        &run,
        &cv,
        std::slice::from_ref(&a),
        "Snapshot gap",
        false,
    )
    .await;
    sqlx::query("UPDATE vacancy SET data=json_set(data,'$.description','Synthetic changed requirements') WHERE id=?").bind(a["id"].as_str()).execute(&t.pool).await.unwrap();
    let current = t
        .get(&format!("vacancies/{}", a["id"].as_str().unwrap()))
        .await["data"]
        .clone();
    assert_eq!(current["updated_at"], a["updated_at"]);
    let rec = t
        .get(&format!(
            "cvs/{}/recommendations",
            cv["id"].as_str().unwrap()
        ))
        .await["items"][0]
        .clone();
    assert_eq!(rec["stale"], true);
    assert_eq!(rec["fresh_vacancy_frequency"], 0);
    let packet = t
        .get(&format!(
            "cv-review-runs/{}/inputs",
            run["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(
        packet["data"]["vacancies"][0]["snapshot"]["description"],
        a["description"]
    );
}
