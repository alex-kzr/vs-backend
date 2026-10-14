//! Explicit CV↔CV / single-CV reviews. No import side effects or model execution.
use crate::{
    cvs,
    error::{ApiError, Result},
    storage as s, validation as v,
};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::HashSet;

pub fn handles(parts: &[&str]) -> bool {
    matches!(
        parts.first(),
        Some(&"cv-review-runs" | &"cv-recommendations")
    ) || matches!(parts, ["cvs", _, "reviews" | "recommendations"])
}
fn version(input: &Value) -> Result<i64> {
    input["expected_version"]
        .as_i64()
        .filter(|v| *v >= 0 && *v < 1_000_000)
        .ok_or_else(|| ApiError::field("expected_version", "Укажите текущую версию состояния."))
}
fn items<'a>(input: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    input[key]
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(|| ApiError::field(key, "Допускается до 100 записей."))
}
fn issue_key(input: &Value) -> Result<String> {
    let key = v::required(input, "issue_key", 100)?;
    if !key
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(ApiError::field(
            "issue_key",
            "Используйте устойчивый ключ из латиницы, цифр, точки, дефиса и подчёркивания.",
        ));
    }
    Ok(key)
}
fn evidence(input: &Value) -> Result<Value> {
    let a = input["evidence"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 300)
        .ok_or_else(|| {
            ApiError::field("evidence", "Приведите от 1 до 300 точных цитат из входов.")
        })?;
    let mut out = Vec::new();
    for e in a {
        v::object(
            e,
            &["revision_id", "vacancy_id", "vacancy_version", "quote"],
        )?;
        let quote = v::required(e, "quote", 2500)?;
        if quote.chars().count() < 5 {
            return Err(ApiError::field(
                "evidence",
                "Цитата должна содержать не менее 5 символов.",
            ));
        }
        let revision = v::optional_uuid(e, "revision_id")?;
        let vacancy = v::optional_uuid(e, "vacancy_id")?;
        if revision.is_null() == vacancy.is_null() {
            return Err(ApiError::field(
                "evidence",
                "Укажите ревизию CV или вакансию.",
            ));
        }
        if vacancy.is_null() {
            if !e.get("vacancy_version").is_none_or(Value::is_null) {
                return Err(ApiError::field(
                    "evidence",
                    "Версия вакансии не относится к цитате CV.",
                ));
            }
            out.push(json!({"revision_id":revision,"quote":quote}));
        } else {
            out.push(json!({"vacancy_id":vacancy,"vacancy_version":v::required(e,"vacancy_version",100)?,"quote":quote}));
        }
    }
    Ok(json!(out))
}
fn selections(input: &Value, cv: &str, reference: &Value) -> Result<(Value, Value)> {
    let mut vacancies = Vec::new();
    let mut contexts = Vec::new();
    let mut seen = HashSet::new();
    if let Some(values) = input.get("vacancies") {
        let a = values
            .as_array()
            .filter(|a| a.len() <= 100)
            .ok_or_else(|| ApiError::field("vacancies", "До 100 вакансий в одном пакете."))?;
        for item in a {
            v::object(item, &["id", "expected_updated_at"])?;
            let id = v::uuid(item, "id")?;
            if !seen.insert(id.clone()) {
                return Err(ApiError::field("vacancies", "Повторяющаяся вакансия."));
            }
            vacancies.push(json!({"id":id,"version":v::time(item,"expected_updated_at")?}));
        }
    }
    if let Some(values) = input.get("context_cvs") {
        let a = values
            .as_array()
            .filter(|a| a.len() <= 20)
            .ok_or_else(|| ApiError::field("context_cvs", "До 20 дополнительных CV."))?;
        for item in a {
            v::object(item, &["id", "expected_revision_id"])?;
            let id = v::uuid(item, "id")?;
            if id == cv || !seen.insert(id.clone()) {
                return Err(ApiError::field("context_cvs", "CV не должны повторяться."));
            }
            contexts.push(json!({"id":id,"revision_id":v::uuid(item,"expected_revision_id")?}));
        }
    }
    if (!vacancies.is_empty() && !reference.is_null())
        || (!contexts.is_empty() && vacancies.is_empty())
    {
        return Err(ApiError::field(
            "vacancies",
            "Сравнение двух CV и пакет вакансий — отдельные проверки.",
        ));
    }
    vacancies.sort_by_key(|x| x["id"].as_str().unwrap_or("").to_owned());
    contexts.sort_by_key(|x| x["id"].as_str().unwrap_or("").to_owned());
    Ok((json!(vacancies), json!(contexts)))
}
fn assessments(input: &Value) -> Result<Value> {
    let empty = json!([]);
    let values = input.get("vacancy_assessments").unwrap_or(&empty);
    let a = values
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(|| ApiError::field("vacancy_assessments", "До 100 оценок."))?;
    let mut out = Vec::new();
    let mut ids = HashSet::new();
    for item in a {
        v::object(
            item,
            &[
                "vacancy_id",
                "vacancy_version",
                "relevance",
                "summary",
                "issue_keys",
            ],
        )?;
        let id = v::uuid(item, "vacancy_id")?;
        if !ids.insert(id.clone()) {
            return Err(ApiError::field(
                "vacancy_assessments",
                "Повторяющаяся оценка.",
            ));
        }
        let relevance =
            v::enum_optional(item, "relevance", &["high", "partial", "low", "unknown"])?;
        if relevance.is_null() {
            return Err(ApiError::field("relevance", "Укажите релевантность."));
        }
        let mut keys = Vec::new();
        for key in items(item, "issue_keys")? {
            keys.push(issue_key(&json!({"issue_key":key}))?);
        }
        out.push(json!({"vacancy_id":id,"vacancy_version":v::required(item,"vacancy_version",100)?,"relevance":relevance,"summary":v::required(item,"summary",3000)?,"issue_keys":keys}));
    }
    Ok(json!(out))
}
pub fn normalize(parts: &[&str], input: &Value) -> Result<Value> {
    match parts {
        ["cv-review-runs"] => {
            v::object(
                input,
                &[
                    "cv_id",
                    "expected_revision_id",
                    "reference_cv_id",
                    "expected_reference_revision_id",
                    "vacancies",
                    "context_cvs",
                    "confirmed",
                ],
            )?;
            if !v::boolean(input, "confirmed", false)? {
                return Err(ApiError::field(
                    "confirmed",
                    "Подтвердите явную проверку CV.",
                ));
            }
            let reference = v::optional_uuid(input, "reference_cv_id")?;
            let revision = v::optional_uuid(input, "expected_reference_revision_id")?;
            if reference.is_null() != revision.is_null() {
                return Err(ApiError::field(
                    "reference_cv_id",
                    "Укажите и CV сравнения, и его ревизию.",
                ));
            }
            let cv = v::uuid(input, "cv_id")?;
            if reference == json!(cv) {
                return Err(ApiError::field(
                    "reference_cv_id",
                    "Выберите другое CV для сравнения.",
                ));
            }
            let (vacancies, contexts) = selections(input, &cv, &reference)?;
            let kind = if !vacancies
                .as_array()
                .ok_or_else(ApiError::internal)?
                .is_empty()
            {
                "vacancy_gap"
            } else if reference.is_null() {
                "single"
            } else {
                "cv_compare"
            };
            Ok(
                json!({"cv_id":cv,"cv_revision_id":v::uuid(input,"expected_revision_id")?,"reference_cv_id":reference,"reference_revision_id":revision,"vacancies":vacancies,"context_cvs":contexts,"kind":kind}),
            )
        }
        ["cv-review-runs", _, "complete"] => {
            v::object(
                input,
                &[
                    "expected_version",
                    "attempt_id",
                    "provenance",
                    "findings",
                    "resolutions",
                    "vacancy_assessments",
                ],
            )?;
            let p = &input["provenance"];
            v::object(p, &["agent", "method", "model"])?;
            let provenance = json!({"agent":v::required(p,"agent",200)?,"method":v::required(p,"method",200)?,"model":v::optional(p,"model",200)?});
            let mut keys = HashSet::new();
            let mut findings = Vec::new();
            let mut resolutions = Vec::new();
            for f in items(input, "findings")? {
                v::object(
                    f,
                    &[
                        "issue_key",
                        "field",
                        "mismatch",
                        "suggested_edit",
                        "rationale",
                        "evidence",
                        "category",
                    ],
                )?;
                let key = issue_key(f)?;
                if !keys.insert(key.clone()) {
                    return Err(ApiError::field(
                        "issue_key",
                        "Ключи замечаний не должны повторяться.",
                    ));
                }
                let field = v::required(f, "field", 100)?;
                if ![
                    "content",
                    "summary",
                    "skills",
                    "experience",
                    "education",
                    "source",
                    "consistency",
                ]
                .contains(&field.as_str())
                {
                    return Err(ApiError::field(
                        "field",
                        "Выберите поле CV: content, summary, skills, experience, education, source, consistency.",
                    ));
                }
                let category = v::enum_optional(
                    f,
                    "category",
                    &[
                        "inconsistent_fact",
                        "omitted_supported_experience",
                        "unconfirmed_requirement",
                        "experience_mismatch",
                        "clarification",
                    ],
                )?;
                let category = if category.is_null() {
                    json!("clarification")
                } else {
                    category
                };
                findings.push(json!({"category":category,"issue_key":key,"field":field,"mismatch":v::required(f,"mismatch",3000)?,"suggested_edit":v::required(f,"suggested_edit",5000)?,"rationale":v::required(f,"rationale",3000)?,"evidence":evidence(f)?}));
            }
            let mut resolved_keys = HashSet::new();
            for r in items(input, "resolutions")? {
                v::object(r, &["issue_key", "rationale", "evidence", "vacancy_ids"])?;
                let key = issue_key(r)?;
                if !resolved_keys.insert(key.clone()) {
                    return Err(ApiError::field(
                        "issue_key",
                        "Подтверждения устранения не должны повторяться.",
                    ));
                }
                let mut vacancy_ids = Vec::new();
                if let Some(values) = r.get("vacancy_ids") {
                    let values = values
                        .as_array()
                        .filter(|a| a.len() <= 100)
                        .ok_or_else(|| ApiError::field("vacancy_ids", "До 100 источников."))?;
                    for id in values {
                        let id = v::uuid(&json!({"id":id}), "id")?;
                        if vacancy_ids.contains(&id) {
                            return Err(ApiError::field("vacancy_ids", "Повторяющийся источник."));
                        }
                        vacancy_ids.push(id);
                    }
                }
                resolutions.push(json!({"issue_key":key,"rationale":v::required(r,"rationale",3000)?,"evidence":evidence(r)?,"vacancy_ids":vacancy_ids}));
            }
            Ok(
                json!({"expected_version":version(input)?,"attempt_id":v::uuid(input,"attempt_id")?,"provenance":provenance,"findings":findings,"resolutions":resolutions,"vacancy_assessments":assessments(input)?}),
            )
        }
        ["cv-review-runs", _, "claim"] => {
            v::object(input, &["expected_version", "agent"])?;
            Ok(json!({"expected_version":version(input)?,"agent":v::required(input,"agent",200)?}))
        }
        ["cv-review-runs", _, "fail"] => {
            v::object(input, &["expected_version", "attempt_id", "error"])?;
            Ok(
                json!({"expected_version":version(input)?,"attempt_id":v::uuid(input,"attempt_id")?,"error":v::required(input,"error",2000)?}),
            )
        }
        ["cv-review-runs", _, "retry" | "cancel"] => {
            v::object(input, &["expected_version"])?;
            Ok(json!({"expected_version":version(input)?}))
        }
        ["cv-recommendations", _, "decision"] => {
            v::object(input, &["expected_version", "state", "reason"])?;
            let state = v::enum_optional(input, "state", &["active", "rejected"])?;
            if state.is_null() {
                return Err(ApiError::field(
                    "state",
                    "Выберите активное или отклонённое состояние.",
                ));
            }
            Ok(
                json!({"expected_version":version(input)?,"state":state,"reason":v::optional(input,"reason",2000)?}),
            )
        }
        _ => Err(ApiError::missing()),
    }
}
async fn scope_recommendations(c: &mut SqliteConnection, scope: &str) -> Result<Vec<Value>> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT data FROM cv_recommendation WHERE scope=? ORDER BY ingest_seq",
    )
    .bind(scope)
    .fetch_all(c)
    .await?;
    rows.into_iter()
        .map(|r| serde_json::from_str(&r).map_err(|_| ApiError::internal()))
        .collect()
}
async fn event(
    c: &mut SqliteConnection,
    table: &str,
    foreign: &str,
    id: &Value,
    run_id: &Value,
    kind: &str,
    data: &Value,
) -> Result<()> {
    let e = s::record(json!({"kind":kind,"snapshot":data}));
    let columns = if table == "cv_review_event" {
        vec![(foreign, id.clone())]
    } else {
        vec![(foreign, id.clone()), ("run_id", run_id.clone())]
    };
    s::insert(c, table, &e, &columns).await
}
async fn staleness(c: &mut SqliteConnection, run: &Value) -> Result<Vec<String>> {
    let mut reasons = Vec::new();
    for (id, revision, label) in [
        ("cv_id", "cv_revision_id", "target_changed"),
        (
            "reference_cv_id",
            "reference_revision_id",
            "reference_changed",
        ),
    ] {
        if let Some(id) = run[id].as_str() {
            let raw = s::get(c, "cv", id).await?;
            let cv = cvs::metadata(c, raw).await?;
            if cv["current_revision_id"] != run[revision] {
                reasons.push(label.into());
            }
            if !cv["deleted_at"].is_null() {
                reasons.push(
                    if label == "target_changed" {
                        "target_deleted"
                    } else {
                        "reference_deleted"
                    }
                    .into(),
                );
            }
        }
    }
    for context in items(run, "context_cvs")? {
        let raw = s::get(
            c,
            "cv",
            context["id"].as_str().ok_or_else(ApiError::internal)?,
        )
        .await?;
        let cv = cvs::metadata(c, raw).await?;
        if cv["current_revision_id"] != context["revision_id"] || !cv["deleted_at"].is_null() {
            reasons.push("context_changed".into());
        }
    }
    if !changed_vacancies(c, run).await?.is_empty() {
        reasons.push("vacancy_changed".into());
    }
    let newer:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM cv_review_run WHERE scope=? AND ingest_seq>(SELECT ingest_seq FROM cv_review_run WHERE id=?) AND json_extract(data,'$.state')='completed' AND json_extract(data,'$.applied')=1)")
 .bind(run["scope"].as_str()).bind(run["id"].as_str()).fetch_one(c).await?;
    if newer && run["kind"] != "vacancy_gap" {
        reasons.push("newer_review".into());
    }
    Ok(reasons)
}
async fn view(c: &mut SqliteConnection, mut run: Value) -> Result<Value> {
    let reasons = staleness(c, &run).await?;
    run["stale"] = json!(!reasons.is_empty());
    run["stale_reasons"] = json!(reasons);
    Ok(run)
}
fn vacancy_snapshot(data: &Value) -> Value {
    let mut snapshot = json!({});
    for field in [
        "id",
        "title",
        "description",
        "company",
        "public_url",
        "location",
        "work_format",
        "employment_type",
        "salary_min",
        "salary_max",
        "salary_currency",
    ] {
        snapshot[field] = data[field].clone();
    }
    snapshot
}
async fn vacancy_inputs(c: &mut SqliteConnection, run: &Value) -> Result<Vec<Value>> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT data FROM cv_review_vacancy WHERE run_id=? ORDER BY ingest_seq",
    )
    .bind(run["id"].as_str())
    .fetch_all(c)
    .await?;
    rows.into_iter()
        .map(|r| serde_json::from_str(&r).map_err(|_| ApiError::internal()))
        .collect()
}
async fn validate_evidence(c: &mut SqliteConnection, run: &Value, input: &Value) -> Result<()> {
    let mut revisions = vec![
        s::get(
            c,
            "cv_revision",
            run["cv_revision_id"]
                .as_str()
                .ok_or_else(ApiError::internal)?,
        )
        .await?,
    ];
    if let Some(id) = run["reference_revision_id"].as_str() {
        revisions.push(s::get(c, "cv_revision", id).await?);
    }
    for context in items(run, "context_cvs")? {
        revisions.push(
            s::get(
                c,
                "cv_revision",
                context["revision_id"]
                    .as_str()
                    .ok_or_else(ApiError::internal)?,
            )
            .await?,
        );
    }
    let vacancies = vacancy_inputs(c, run).await?;
    let mut seen = HashSet::new();
    let mut seen_vacancy = false;
    for e in input["evidence"]
        .as_array()
        .ok_or_else(ApiError::internal)?
    {
        let text = if let Some(id) = e["revision_id"].as_str() {
            let r = revisions
                .iter()
                .find(|r| r["id"] == id)
                .ok_or_else(|| ApiError::field("evidence", "Посторонняя ревизия CV."))?;
            seen.insert(id.to_owned());
            r["content"]
                .as_str()
                .ok_or_else(ApiError::internal)?
                .to_owned()
        } else {
            let vacancy = vacancies
                .iter()
                .find(|r| {
                    r["vacancy_id"] == e["vacancy_id"] && r["version"] == e["vacancy_version"]
                })
                .ok_or_else(|| ApiError::field("evidence", "Посторонняя вакансия или версия."))?;
            seen_vacancy = true;
            format!(
                "{}\n{}",
                vacancy["snapshot"]["title"].as_str().unwrap_or(""),
                vacancy["snapshot"]["description"].as_str().unwrap_or("")
            )
        };
        if !text.contains(e["quote"].as_str().ok_or_else(ApiError::internal)?) {
            return Err(ApiError::field(
                "evidence",
                "Цитата не найдена в зафиксированном входе.",
            ));
        }
    }
    if !seen.contains(
        run["cv_revision_id"]
            .as_str()
            .ok_or_else(ApiError::internal)?,
    ) || (run["kind"] == "cv_compare"
        && !seen.contains(
            run["reference_revision_id"]
                .as_str()
                .ok_or_else(ApiError::internal)?,
        ))
        || (run["kind"] == "vacancy_gap" && !seen_vacancy)
    {
        return Err(ApiError::field(
            "evidence",
            "Нужны цитаты целевого CV и второго CV / вакансии при сравнении.",
        ));
    }
    if input["category"] == "omitted_supported_experience" && seen.len() < 2 {
        return Err(ApiError::field(
            "category",
            "Опыт из другого CV требует цитаты из этого CV.",
        ));
    }
    if input["category"] == "inconsistent_fact" && seen.len() < 2 {
        return Err(ApiError::field(
            "category",
            "Противоречие требует двух ревизий CV.",
        ));
    }
    Ok(())
}
fn validate_mixed(run: &Value, input: &Value) -> Result<()> {
    for resolution in items(input, "resolutions")? {
        if let Some(finding) = items(input, "findings")?
            .iter()
            .find(|f| f["issue_key"] == resolution["issue_key"])
        {
            let ids = items(resolution, "vacancy_ids")?;
            if run["kind"] != "vacancy_gap" || ids.is_empty() {
                return Err(ApiError::field(
                    "issue_key",
                    "Одновременные finding/resolution допустимы только для разных явно указанных вакансий.",
                ));
            }
            if finding["evidence"]
                .as_array()
                .ok_or_else(ApiError::internal)?
                .iter()
                .any(|e| ids.iter().any(|id| e["vacancy_id"] == *id))
            {
                return Err(ApiError::field(
                    "issue_key",
                    "Нельзя одновременно подтвердить и устранить пробел одной вакансии.",
                ));
            }
        }
    }
    Ok(())
}
async fn validate_coverage(c: &mut SqliteConnection, run: &Value, input: &Value) -> Result<()> {
    let vacancies = vacancy_inputs(c, run).await?;
    let reports = items(input, "vacancy_assessments")?;
    if reports.len() != vacancies.len() {
        return Err(ApiError::field(
            "vacancy_assessments",
            "Требуется оценка каждой вакансии пакета.",
        ));
    }
    for vacancy in &vacancies {
        let report = reports
            .iter()
            .find(|r| {
                r["vacancy_id"] == vacancy["vacancy_id"]
                    && r["vacancy_version"] == vacancy["version"]
            })
            .ok_or_else(|| {
                ApiError::field("vacancy_assessments", "Оценка не соответствует входам.")
            })?;
        for key in items(report, "issue_keys")? {
            let f = items(input, "findings")?
                .iter()
                .find(|f| f["issue_key"] == *key)
                .ok_or_else(|| {
                    ApiError::field(
                        "issue_keys",
                        "Укажите существующий ключ замечания результата.",
                    )
                })?;
            if !f["evidence"]
                .as_array()
                .ok_or_else(ApiError::internal)?
                .iter()
                .any(|e| e["vacancy_id"] == vacancy["vacancy_id"])
            {
                return Err(ApiError::field(
                    "issue_keys",
                    "Замечание должно цитировать эту вакансию.",
                ));
            }
        }
    }
    for finding in items(input, "findings")? {
        for e in finding["evidence"]
            .as_array()
            .ok_or_else(ApiError::internal)?
        {
            if let Some(id) = e["vacancy_id"].as_str() {
                let report = reports
                    .iter()
                    .find(|r| r["vacancy_id"] == id)
                    .ok_or_else(|| {
                        ApiError::field("vacancy_assessments", "Отсутствует оценка вакансии.")
                    })?;
                if !items(report, "issue_keys")?
                    .iter()
                    .any(|key| *key == finding["issue_key"])
                {
                    return Err(ApiError::field(
                        "issue_keys",
                        "Оценка должна включать процитированное замечание.",
                    ));
                }
            }
        }
    }
    Ok(())
}
async fn save_recommendation(
    c: &mut SqliteConnection,
    run: &Value,
    mut rec: Value,
    kind: &str,
    new: bool,
) -> Result<()> {
    rec["version"] = json!(rec["version"].as_i64().unwrap_or(0) + 1);
    rec["updated_at"] = json!(v::now());
    let columns = [
        ("cv_id", run["cv_id"].clone()),
        ("reference_cv_id", run["reference_cv_id"].clone()),
        ("scope", run["scope"].clone()),
        ("issue_key", rec["issue_key"].clone()),
    ];
    if new {
        s::insert(c, "cv_recommendation", &rec, &columns).await?;
    } else {
        s::update(c, "cv_recommendation", &rec, &[]).await?;
    }
    event(
        c,
        "cv_recommendation_event",
        "recommendation_id",
        &rec["id"],
        &run["id"],
        kind,
        &rec,
    )
    .await
}
async fn run_sequence(c: &mut SqliteConnection, id: &Value) -> Result<i64> {
    sqlx::query_scalar("SELECT ingest_seq FROM cv_review_run WHERE id=?")
        .bind(id.as_str())
        .fetch_one(c)
        .await
        .map_err(Into::into)
}
async fn superseded(c: &mut SqliteConnection, run: &Value) -> Result<Vec<String>> {
    if run["kind"] != "vacancy_gap" {
        return Ok(Vec::new());
    }
    sqlx::query_scalar("SELECT DISTINCT v.vacancy_id FROM cv_review_vacancy v JOIN cv_review_run r ON r.id=v.run_id WHERE r.scope=? AND r.ingest_seq>(SELECT ingest_seq FROM cv_review_run WHERE id=?) AND json_extract(r.data,'$.state')='completed' AND json_extract(r.data,'$.applied')=1 AND v.vacancy_id IN(SELECT vacancy_id FROM cv_review_vacancy WHERE run_id=?)")
 .bind(run["scope"].as_str()).bind(run["id"].as_str()).bind(run["id"].as_str()).fetch_all(c).await.map_err(Into::into)
}
async fn changed_vacancies(c: &mut SqliteConnection, run: &Value) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for vacancy in vacancy_inputs(c, run).await? {
        let id = vacancy["vacancy_id"]
            .as_str()
            .ok_or_else(ApiError::internal)?;
        let current = s::get(c, "vacancy", id).await?;
        let version_changed = current["updated_at"] != vacancy["version"];
        let public = s::hydrate(c, "vacancy", current, false).await?;
        if version_changed || vacancy_snapshot(&public) != vacancy["snapshot"] {
            changed.push(id.to_owned());
        }
    }
    Ok(changed)
}
fn blocking_reasons(run: &Value, reasons: &[String]) -> bool {
    reasons
        .iter()
        .any(|reason| run["kind"] != "vacancy_gap" || reason != "vacancy_changed")
}
async fn owned_evidence(c: &mut SqliteConnection, evidence: &Value) -> Result<Value> {
    let mut out = Vec::new();
    for e in evidence.as_array().ok_or_else(ApiError::internal)? {
        let mut e = e.clone();
        if let Some(id) = e["revision_id"].as_str() {
            let revision = s::get(c, "cv_revision", id).await?;
            e["cv_id"] = revision["cv_id"].clone();
        }
        out.push(e);
    }
    Ok(json!(out))
}
async fn source_fresh(c: &mut SqliteConnection, target: &Value, source: &Value) -> Result<bool> {
    let raw = s::get(c, "cv", target.as_str().ok_or_else(ApiError::internal)?).await?;
    let cv = cvs::metadata(c, raw).await?;
    if cv["current_revision_id"] != source["cv_revision_id"] || !cv["deleted_at"].is_null() {
        return Ok(false);
    }
    let vacancy = s::get(
        c,
        "vacancy",
        source["vacancy_id"]
            .as_str()
            .ok_or_else(ApiError::internal)?,
    )
    .await?;
    let version = source.get("vacancy_version").unwrap_or(&source["version"]);
    if vacancy["updated_at"] != *version {
        return Ok(false);
    }
    let pinned: String =
        sqlx::query_scalar("SELECT data FROM cv_review_vacancy WHERE run_id=? AND vacancy_id=?")
            .bind(source["run_id"].as_str())
            .bind(source["vacancy_id"].as_str())
            .fetch_one(&mut *c)
            .await?;
    let pinned: Value = serde_json::from_str(&pinned).map_err(|_| ApiError::internal())?;
    let public = s::hydrate(c, "vacancy", vacancy, false).await?;
    if vacancy_snapshot(&public) != pinned["snapshot"] {
        return Ok(false);
    }
    for context in source["context_cvs"].as_array().into_iter().flatten() {
        let raw = s::get(
            c,
            "cv",
            context["id"].as_str().ok_or_else(ApiError::internal)?,
        )
        .await?;
        let cv = cvs::metadata(c, raw).await?;
        if cv["current_revision_id"] != context["revision_id"] || !cv["deleted_at"].is_null() {
            return Ok(false);
        }
    }
    Ok(true)
}
async fn recommendation_view(c: &mut SqliteConnection, mut rec: Value) -> Result<Value> {
    let run = s::get(
        c,
        "cv_review_run",
        rec["last_run_id"].as_str().ok_or_else(ApiError::internal)?,
    )
    .await?;
    let mut stale = blocking_reasons(&run, &staleness(c, &run).await?);
    let mut fresh = HashSet::new();
    let target = rec["cv_id"].clone();
    let checks = rec["source_resolutions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if let Some(sources) = rec["vacancy_evidence"].as_array_mut() {
        for source in sources {
            let proof = checks
                .iter()
                .find(|r| r["vacancy_id"] == source["vacancy_id"])
                .unwrap_or(source);
            let is_fresh = source_fresh(c, &target, proof).await?;
            source["stale"] = json!(!is_fresh);
            if is_fresh {
                fresh.insert(
                    source["vacancy_id"]
                        .as_str()
                        .ok_or_else(ApiError::internal)?
                        .to_owned(),
                );
            } else {
                stale = true;
            }
        }
    }
    rec["fresh_vacancy_frequency"] = json!(fresh.len());
    rec["stale"] = json!(stale);
    Ok(rec)
}
async fn apply(c: &mut SqliteConnection, run: &Value, input: &Value) -> Result<()> {
    let mut existing =
        scope_recommendations(c, run["scope"].as_str().ok_or_else(ApiError::internal)?).await?;
    let mut superseded = superseded(c, run).await?;
    superseded.extend(changed_vacancies(c, run).await?);
    let seq = run_sequence(c, &run["id"]).await?;
    for f in items(input, "findings")? {
        let mut finding = f.clone();
        if run["kind"] == "vacancy_gap" {
            finding["evidence"]
                .as_array_mut()
                .ok_or_else(ApiError::internal)?
                .retain(|e| {
                    e["vacancy_id"]
                        .as_str()
                        .is_none_or(|id| !superseded.iter().any(|old| old == id))
                });
            if !finding["evidence"]
                .as_array()
                .ok_or_else(ApiError::internal)?
                .iter()
                .any(|e| e["vacancy_id"].is_string())
            {
                continue;
            }
        }
        let f = &finding;
        let previous = existing
            .iter()
            .find(|r| r["issue_key"] == f["issue_key"])
            .cloned();
        let new = previous.is_none();
        let mut rec=previous.unwrap_or_else(||s::record(json!({"cv_id":run["cv_id"],"reference_cv_id":run["reference_cv_id"],"scope":run["scope"],"origin_run_id":run["id"],"state":"active","version":0,"decision_reason":null,"source_resolutions":[]})));
        let description_run = rec.get("description_run_id").unwrap_or(&rec["last_run_id"]);
        let replace_description = new || seq >= run_sequence(c, description_run).await?;
        let mut sources = rec["vacancy_evidence"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let ids: HashSet<String> = f["evidence"]
            .as_array()
            .ok_or_else(ApiError::internal)?
            .iter()
            .filter_map(|e| e["vacancy_id"].as_str().map(str::to_owned))
            .collect();
        sources.retain(|old| {
            !old["vacancy_id"]
                .as_str()
                .is_some_and(|id| ids.contains(id))
        });
        for e in f["evidence"].as_array().ok_or_else(ApiError::internal)? {
            if e["vacancy_id"].is_string() {
                let mut source = e.clone();
                source["run_id"] = run["id"].clone();
                source["cv_revision_id"] = run["cv_revision_id"].clone();
                source["context_cvs"] = run["context_cvs"].clone();
                sources.push(source);
            }
        }
        rec["vacancy_frequency"] = json!(
            sources
                .iter()
                .filter_map(|e| e["vacancy_id"].as_str())
                .collect::<HashSet<_>>()
                .len()
        );
        rec["vacancy_evidence"] = json!(sources);
        if let Some(resolutions) = rec["source_resolutions"].as_array_mut() {
            resolutions.retain(|r| !r["vacancy_id"].as_str().is_some_and(|id| ids.contains(id)));
        }
        if replace_description {
            for (key, value) in f.as_object().ok_or_else(ApiError::internal)? {
                rec[key] = value.clone();
            }
            rec["evidence"] = owned_evidence(c, &f["evidence"]).await?;
            rec["description_run_id"] = run["id"].clone();
        }
        rec["kind"] = run["kind"].clone();
        rec["last_run_id"] = run["id"].clone();
        rec["cv_revision_id"] = run["cv_revision_id"].clone();
        rec["reference_revision_id"] = run["reference_revision_id"].clone();
        if rec["state"] != "rejected" {
            rec["state"] = json!("active");
        }
        save_recommendation(c, run, rec, "finding", new).await?;
    }
    existing =
        scope_recommendations(c, run["scope"].as_str().ok_or_else(ApiError::internal)?).await?;
    for resolution in items(input, "resolutions")? {
        let rec = existing
            .iter_mut()
            .find(|r| r["issue_key"] == resolution["issue_key"])
            .ok_or_else(|| {
                ApiError::field(
                    "resolutions",
                    "Закрыть можно только существующее замечание.",
                )
            })?;
        if rec["state"] == "rejected" {
            continue;
        }
        let mut enriched = resolution.clone();
        enriched["evidence"] = owned_evidence(c, &resolution["evidence"]).await?;
        if run["kind"] == "vacancy_gap" {
            let source_ids: HashSet<String> = rec["vacancy_evidence"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["vacancy_id"].as_str().map(str::to_owned))
                .collect();
            let requested = items(resolution, "vacancy_ids")?;
            let ids: Vec<String> = if requested.is_empty() {
                items(run, "vacancies")?
                    .iter()
                    .filter_map(|r| r["id"].as_str())
                    .filter(|id| source_ids.contains(*id))
                    .map(str::to_owned)
                    .collect()
            } else {
                requested
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            };
            if ids.is_empty() {
                return Err(ApiError::field(
                    "resolutions",
                    "В пакете нет источников этого замечания.",
                ));
            }
            let mut checks = rec["source_resolutions"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for id in ids {
                if superseded.contains(&id) {
                    continue;
                }
                let vacancy = items(run, "vacancies")?
                    .iter()
                    .find(|v| v["id"] == id)
                    .ok_or_else(|| ApiError::field("resolutions", "Источник не входит в пакет."))?;
                if !source_ids.contains(&id)
                    || !resolution["evidence"]
                        .as_array()
                        .ok_or_else(ApiError::internal)?
                        .iter()
                        .any(|e| e["vacancy_id"] == id)
                {
                    return Err(ApiError::field(
                        "resolutions",
                        "Нужно доказательство проверки каждого указанного источника.",
                    ));
                }
                checks.retain(|r| r["vacancy_id"] != id);
                checks.push(json!({"vacancy_id":id,"version":vacancy["version"],"cv_revision_id":run["cv_revision_id"],"run_id":run["id"],"context_cvs":run["context_cvs"],"rationale":resolution["rationale"],"evidence":enriched["evidence"].as_array().ok_or_else(ApiError::internal)?.iter().filter(|e|e["revision_id"].is_string()||e["vacancy_id"]==id).cloned().collect::<Vec<_>>()}));
            }
            let mut all = true;
            for id in source_ids {
                if let Some(check) = checks.iter().find(|r| r["vacancy_id"] == id) {
                    if !source_fresh(c, &run["cv_id"], check).await? {
                        all = false;
                    }
                } else {
                    all = false;
                }
            }
            rec["source_resolutions"] = json!(checks);
            rec["state"] = json!(if all { "resolved" } else { "active" });
        } else {
            rec["state"] = json!("resolved");
        }
        rec["resolution"] = enriched;
        rec["last_run_id"] = run["id"].clone();
        rec["cv_revision_id"] = run["cv_revision_id"].clone();
        rec["reference_revision_id"] = run["reference_revision_id"].clone();
        save_recommendation(c, run, rec.clone(), "resolution_checked", false).await?;
    }
    Ok(())
}
pub async fn write(c: &mut SqliteConnection, parts: &[&str], input: &Value) -> Result<Value> {
    if parts == ["cv-review-runs"] {
        for (id, revision) in [
            ("cv_id", "cv_revision_id"),
            ("reference_cv_id", "reference_revision_id"),
        ] {
            if let Some(id) = input[id].as_str() {
                cvs::require_active(c, id).await?;
                let cv = s::get(c, "cv", id).await?;
                if cv["current_revision_id"] != input[revision] {
                    return Err(ApiError::conflict("cv_revision_conflict"));
                }
            }
        }
        for context in items(input, "context_cvs")? {
            let id = context["id"].as_str().ok_or_else(ApiError::internal)?;
            cvs::require_active(c, id).await?;
            let cv = s::get(c, "cv", id).await?;
            if cv["current_revision_id"] != context["revision_id"] {
                return Err(ApiError::conflict("cv_revision_conflict"));
            }
        }
        let mut pinned = Vec::new();
        for selection in items(input, "vacancies")? {
            let id = selection["id"].as_str().ok_or_else(ApiError::internal)?;
            let raw = s::get(c, "vacancy", id).await?;
            if raw["updated_at"] != selection["version"] {
                return Err(ApiError::conflict("vacancy_version_conflict"));
            }
            let public = s::hydrate(c, "vacancy", raw, false).await?;
            let snapshot = vacancy_snapshot(&public);
            pinned.push(s::record(
                json!({"vacancy_id":id,"version":selection["version"],"snapshot":snapshot}),
            ));
        }
        let scope = format!(
            "{}:{}",
            input["cv_id"].as_str().ok_or_else(ApiError::internal)?,
            if input["kind"] == "vacancy_gap" {
                "vacancies"
            } else {
                input["reference_cv_id"].as_str().unwrap_or("single")
            }
        );
        let previous = scope_recommendations(c, &scope).await?;
        if previous.len() > 500 {
            return Err(ApiError::field(
                "cv_id",
                "В этой области больше 500 замечаний; требуется отдельная стратегия проверки.",
            ));
        }
        let mut run = s::record(input.clone());
        run["scope"] = json!(scope);
        run["state"] = json!("queued");
        run["version"] = json!(0);
        run["attempt_id"] = Value::Null;
        run["attempt_number"] = json!(0);
        run["previous_recommendations"] = json!(previous);
        s::insert(
            c,
            "cv_review_run",
            &run,
            &[
                ("cv_id", run["cv_id"].clone()),
                ("cv_revision_id", run["cv_revision_id"].clone()),
                ("reference_cv_id", run["reference_cv_id"].clone()),
                (
                    "reference_revision_id",
                    run["reference_revision_id"].clone(),
                ),
                ("scope", run["scope"].clone()),
            ],
        )
        .await?;
        for vacancy in pinned {
            s::insert(
                c,
                "cv_review_vacancy",
                &vacancy,
                &[
                    ("run_id", run["id"].clone()),
                    ("vacancy_id", vacancy["vacancy_id"].clone()),
                ],
            )
            .await?;
        }
        event(
            c,
            "cv_review_event",
            "run_id",
            &run["id"],
            &run["id"],
            "requested",
            &run,
        )
        .await?;
        return view(c, run).await;
    }
    if let ["cv-recommendations", id, "decision"] = parts {
        let mut rec = s::get(c, "cv_recommendation", id).await?;
        if rec["version"] != input["expected_version"] {
            return Err(ApiError::conflict("recommendation_version_conflict"));
        }
        if rec["state"] == "resolved" {
            return Err(ApiError::conflict("recommendation_resolved"));
        }
        rec["state"] = input["state"].clone();
        rec["decision_reason"] = input["reason"].clone();
        let run = s::get(
            c,
            "cv_review_run",
            rec["last_run_id"].as_str().ok_or_else(ApiError::internal)?,
        )
        .await?;
        save_recommendation(c, &run, rec, "user_decision", false).await?;
        return s::get(c, "cv_recommendation", id).await;
    }
    let ["cv-review-runs", id, action] = parts else {
        return Err(ApiError::missing());
    };
    let mut run = s::get(c, "cv_review_run", id).await?;
    if run["version"] != input["expected_version"] {
        return Err(ApiError::conflict("review_version_conflict"));
    }
    let state = run["state"].as_str().ok_or_else(ApiError::internal)?;
    match *action {
        "claim" if state == "queued" => {
            run["state"] = json!("running");
            run["attempt_id"] = json!(v::id());
            run["attempt_number"] = json!(run["attempt_number"].as_i64().unwrap_or(0) + 1);
            run["agent"] = input["agent"].clone();
        }
        "complete" | "fail" if state == "running" => {
            if run["attempt_id"] != input["attempt_id"] {
                return Err(ApiError::conflict("review_attempt_conflict"));
            }
            if *action == "fail" {
                run["state"] = json!("failed");
                run["error"] = input["error"].clone();
            } else {
                if run["agent"] != input["provenance"]["agent"] {
                    return Err(ApiError::field(
                        "provenance",
                        "Результат должен принадлежать агенту, принявшему проверку.",
                    ));
                }
                for f in items(input, "findings")?
                    .iter()
                    .chain(items(input, "resolutions")?)
                {
                    validate_evidence(c, &run, f).await?;
                }
                for resolution in items(input, "resolutions")? {
                    if !run["previous_recommendations"]
                        .as_array()
                        .ok_or_else(ApiError::internal)?
                        .iter()
                        .any(|r| r["issue_key"] == resolution["issue_key"])
                    {
                        return Err(ApiError::field(
                            "resolutions",
                            "Замечание отсутствует во входном пакете этой проверки.",
                        ));
                    }
                }
                validate_mixed(&run, input)?;
                validate_coverage(c, &run, input).await?;
                let stale = staleness(c, &run).await?;
                let superseded = superseded(c, &run).await?;
                let changed = changed_vacancies(c, &run).await?;
                let any_current = run["kind"] != "vacancy_gap"
                    || items(&run, "vacancies")?.iter().any(|v| {
                        !superseded
                            .iter()
                            .chain(changed.iter())
                            .any(|id| v["id"] == *id)
                    });
                run["superseded_vacancy_ids"] = json!(superseded);
                run["stale_vacancy_ids"] = json!(changed);
                let can_apply = !blocking_reasons(&run, &stale) && any_current;
                run["applied"] = json!(can_apply);
                run["result"] = input.clone();
                run["state"] = json!("completed");
                if can_apply {
                    apply(c, &run, input).await?;
                }
            }
        }
        "retry" if state == "failed" => {
            if !staleness(c, &run).await?.is_empty() {
                return Err(ApiError::conflict("review_stale"));
            }
            run["state"] = json!("queued");
            run["attempt_id"] = Value::Null;
            run["error"] = Value::Null;
        }
        "cancel" if ["queued", "running", "failed"].contains(&state) => {
            run["state"] = json!("cancelled");
        }
        _ => return Err(ApiError::conflict("review_state_conflict")),
    }
    run["version"] = json!(run["version"].as_i64().ok_or_else(ApiError::internal)? + 1);
    run["updated_at"] = json!(v::now());
    s::update(c, "cv_review_run", &run, &[]).await?;
    event(
        c,
        "cv_review_event",
        "run_id",
        &run["id"],
        &run["id"],
        action,
        &run,
    )
    .await?;
    view(c, run).await
}
async fn page(
    c: &mut SqliteConnection,
    table: &str,
    column: &str,
    id: &str,
    query: &str,
) -> Result<Value> {
    let mut cursor = i64::MAX;
    let mut limit = 100i64;
    let mut seen = HashSet::new();
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if !seen.insert(key.to_string()) {
            return Err(ApiError::field("query", "Повторяющийся параметр."));
        }
        match key.as_ref() {
            "cursor" => {
                cursor = value
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| ApiError::field("cursor", "Некорректный курсор."))?
            }
            "limit" => {
                limit = value
                    .parse()
                    .ok()
                    .filter(|n| *n > 0 && *n <= 100)
                    .ok_or_else(|| ApiError::field("limit", "Лимит от 1 до 100."))?
            }
            _ => return Err(ApiError::field("query", "Неизвестный параметр.")),
        }
    }
    let rows=sqlx::query(sqlx::AssertSqlSafe(format!("SELECT ingest_seq,data FROM {table} WHERE {column}=? AND ingest_seq<? ORDER BY ingest_seq DESC LIMIT ?")))
 .bind(id).bind(cursor).bind(limit+1).fetch_all(&mut *c).await?;
    let more = rows.len() > limit as usize;
    let mut out = Vec::new();
    let mut last = 0;
    for row in rows.into_iter().take(limit as usize) {
        last = row.get::<i64, _>("ingest_seq");
        let raw: Value = serde_json::from_str(row.get("data")).map_err(|_| ApiError::internal())?;
        out.push(if table == "cv_review_run" {
            view(c, raw).await?
        } else if table == "cv_recommendation" {
            recommendation_view(c, raw).await?
        } else {
            raw
        });
    }
    Ok(json!({"items":out,"next_cursor":if more{Some(last.to_string())}else{None}}))
}
pub async fn read(c: &mut SqliteConnection, parts: &[&str], query: &str) -> Result<Value> {
    match parts {
        ["cvs", id, "reviews"] | ["cvs", id, "recommendations"] => {
            s::get(c, "cv", id).await?;
            page(
                c,
                if parts[2] == "reviews" {
                    "cv_review_run"
                } else {
                    "cv_recommendation"
                },
                "cv_id",
                id,
                query,
            )
            .await
        }
        ["cv-review-runs", id, "history"] => {
            s::get(c, "cv_review_run", id).await?;
            page(c, "cv_review_event", "run_id", id, query).await
        }
        ["cv-recommendations", id, "history"] => {
            s::get(c, "cv_recommendation", id).await?;
            page(c, "cv_recommendation_event", "recommendation_id", id, query).await
        }
        ["cv-review-runs", id] | ["cv-review-runs", id, "inputs"] => {
            if !query.is_empty() {
                return Err(ApiError::field(
                    "query",
                    "Параметры здесь не поддерживаются.",
                ));
            }
            let run = s::get(c, "cv_review_run", id).await?;
            if parts.len() == 2 {
                return Ok(json!({"data":view(c,run).await?}));
            }
            let target = s::get(
                c,
                "cv_revision",
                run["cv_revision_id"]
                    .as_str()
                    .ok_or_else(ApiError::internal)?,
            )
            .await?;
            let reference = if let Some(id) = run["reference_revision_id"].as_str() {
                s::get(c, "cv_revision", id).await?
            } else {
                Value::Null
            };
            Ok(
                json!({"data":{"run":view(c,run.clone()).await?,"target_revision":target,"reference_revision":reference,"previous_recommendations":run["previous_recommendations"],"vacancies":vacancy_inputs(c,&run).await?,"context_revisions":context_inputs(c,&run).await?,
    "instructions":"Проверяйте только явно запрошенные входы. Целевое CV проверяется относительно reference_revision. Не выдумывайте опыт, навыки, образование и факты. Предлагайте уточнения с условием подтверждения пользователем. Цитируйте точный текст content обеих ревизий для сравнения. Сохраняйте issue_key известных замечаний. Устранение подтверждайте явно в resolutions после повторной проверки; пропуск замечания не закрывает его. Не редактируйте внешние сайты и не отправляйте отклики."}}),
            )
        }
        _ => Err(ApiError::missing()),
    }
}

async fn context_inputs(c: &mut SqliteConnection, run: &Value) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    for context in items(run, "context_cvs")? {
        out.push(
            s::get(
                c,
                "cv_revision",
                context["revision_id"]
                    .as_str()
                    .ok_or_else(ApiError::internal)?,
            )
            .await?,
        );
    }
    Ok(out)
}
