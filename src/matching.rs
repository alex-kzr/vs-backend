//! Explicit, durable external-agent batches. No matcher, scheduler or provider calls.
use crate::{
    error::{ApiError, Result},
    storage as s, validation as v,
};
use serde_json::{Value, json};
use sqlx::SqliteConnection;
const DIMENSIONS: &[&str] = &[
    "skills",
    "experience",
    "responsibilities",
    "work_format",
    "language",
    "education",
];

fn strings(input: &Value, key: &str, ids: bool) -> Result<Value> {
    let empty = json!([]);
    let value = input.get(key).unwrap_or(&empty);
    let array = value
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(|| ApiError::field(key, "Допускается до 100 значений."))?;
    let mut values = Vec::new();
    for item in array {
        let text = if ids {
            v::uuid(&json!({key:item}), key)?
        } else {
            v::required(&json!({key:item}), key, 100)?
        };
        if !ids && !v::STATUSES.contains(&text.as_str()) {
            return Err(ApiError::field(key, "Недопустимый статус вакансии."));
        }
        if values.contains(&text) {
            return Err(ApiError::field(key, "Повторяющееся значение."));
        }
        values.push(text);
    }
    values.sort();
    Ok(json!(values))
}
pub fn normalize_selection(input: &Value, confirmed: bool) -> Result<Value> {
    let mut allowed = vec!["cv_id", "expected_revision_id", "selection"];
    if confirmed {
        allowed.push("confirmed");
    }
    v::object(input, &allowed)?;
    if confirmed && !v::boolean(input, "confirmed", false)? {
        return Err(ApiError::field(
            "confirmed",
            "Подтвердите создание выбранного пакета.",
        ));
    }
    let sel = &input["selection"];
    v::object(
        sel,
        &[
            "statuses",
            "company_ids",
            "vacancy_ids",
            "q",
            "source_id",
            "search_configuration_id",
        ],
    )?;
    let selection = json!({"statuses":strings(sel,"statuses",false)?,"company_ids":strings(sel,"company_ids",true)?,
  "vacancy_ids":strings(sel,"vacancy_ids",true)?,"q":v::optional(sel,"q",200)?,"source_id":v::optional_uuid(sel,"source_id")?,
  "search_configuration_id":v::optional_uuid(sel,"search_configuration_id")?});
    if !selection["vacancy_ids"]
        .as_array()
        .ok_or_else(ApiError::internal)?
        .is_empty()
        && (!selection["statuses"]
            .as_array()
            .ok_or_else(ApiError::internal)?
            .is_empty()
            || !selection["company_ids"]
                .as_array()
                .ok_or_else(ApiError::internal)?
                .is_empty()
            || !selection["q"].is_null()
            || !selection["source_id"].is_null()
            || !selection["search_configuration_id"].is_null())
    {
        return Err(ApiError::field(
            "selection",
            "Выберите либо список ID, либо фильтры, без смешивания.",
        ));
    }
    Ok(
        json!({"cv_id":v::uuid(input,"cv_id")?,"expected_revision_id":v::uuid(input,"expected_revision_id")?,"selection":selection}),
    )
}
async fn selected(c: &mut SqliteConnection, input: &Value) -> Result<(Value, Vec<Value>)> {
    let cv = s::get(
        c,
        "cv",
        input["cv_id"].as_str().ok_or_else(ApiError::internal)?,
    )
    .await?;
    if cv["current_revision_id"] != input["expected_revision_id"] {
        return Err(ApiError::conflict("cv_revision_conflict"));
    }
    let sel = &input["selection"];
    let mut items = Vec::new();
    let ids = sel["vacancy_ids"]
        .as_array()
        .ok_or_else(ApiError::internal)?;
    if !ids.is_empty() {
        for id in ids {
            let data = s::get(c, "vacancy", id.as_str().ok_or_else(ApiError::internal)?).await?;
            items.push(s::hydrate(c, "vacancy", data, false).await?);
        }
    } else {
        let query = {
            let mut q = url::form_urlencoded::Serializer::new(String::new());
            q.append_pair("limit", "100");
            for (field, parameter) in [("statuses", "status"), ("company_ids", "company_id")] {
                for value in sel[field].as_array().ok_or_else(ApiError::internal)? {
                    q.append_pair(parameter, value.as_str().ok_or_else(ApiError::internal)?);
                }
            }
            for field in ["q", "source_id", "search_configuration_id"] {
                if let Some(value) = sel[field].as_str() {
                    q.append_pair(field, value);
                }
            }
            q.finish()
        };
        let page = s::read(c, &["vacancies"], &query).await?;
        if !page["next_cursor"].is_null() {
            return Err(ApiError::field(
                "selection",
                "В пакете допускается до 100 вакансий. Уточните выборку.",
            ));
        }
        items = page["items"]
            .as_array()
            .ok_or_else(ApiError::internal)?
            .clone();
    }
    Ok((cv, items))
}
pub async fn preview(c: &mut SqliteConnection, input: &Value) -> Result<Value> {
    let (cv, items) = selected(c, input).await?;
    let vacancies:Vec<_>=items.iter().map(|r|json!({"id":r["id"],"title":r["title"],"company_name":r["company"]["name"],"status":r["status"]})).collect();
    Ok(
        json!({"cv_id":cv["id"],"cv_revision_id":cv["current_revision_id"],"cv_revision_number":cv["current_revision_number"],"cv_name":cv["name"],"selection":input["selection"],"count":items.len(),"vacancies":vacancies}),
    )
}
fn snapshot(data: &Value) -> Value {
    let mut fields = serde_json::Map::new();
    for key in [
        "id",
        "title",
        "description",
        "location",
        "work_format",
        "employment_type",
        "salary_min",
        "salary_max",
        "salary_currency",
        "public_url",
        "company",
    ] {
        fields.insert(key.into(), data[key].clone());
    }
    Value::Object(fields)
}
async fn current_snapshot(c: &mut SqliteConnection, id: &str) -> Result<Value> {
    let data = s::get(c, "vacancy", id).await?;
    let mut data = data;
    let company = s::get(
        c,
        "company",
        data["company_id"].as_str().ok_or_else(ApiError::internal)?,
    )
    .await?;
    data["company"] = json!({"id":company["id"],"name":company["name"]});
    Ok(snapshot(&data))
}
fn engine(input: &Value) -> Result<Value> {
    v::object(input, &["kind", "agent", "provider", "model", "version"])?;
    let kind = v::required(input, "kind", 30)?;
    if !["manual", "llm", "synthetic"].contains(&kind.as_str()) {
        return Err(ApiError::field(
            "engine",
            "Укажите manual, llm или synthetic.",
        ));
    }
    let provider = v::optional(input, "provider", 200)?;
    let model = v::optional(input, "model", 200)?;
    if kind == "llm" && (provider.is_null() || model.is_null()) {
        return Err(ApiError::field(
            "engine",
            "Для LLM результата укажите сообщённые агентом provider и model.",
        ));
    }
    Ok(
        json!({"kind":kind,"agent":v::required(input,"agent",200)?,"provider":provider,"model":model,"version":v::optional(input,"version",200)?}),
    )
}
fn assessment(input: &Value) -> Result<Value> {
    v::object(
        input,
        &["summary", "matches", "gaps", "questions", "unknowns"],
    )?;
    let mut output = json!({"summary":v::required(input,"summary",8000)?});
    for group in ["matches", "gaps", "questions", "unknowns"] {
        let items = input[group]
            .as_array()
            .filter(|a| a.len() <= 40)
            .ok_or_else(|| {
                ApiError::field(
                    "assessment",
                    "Каждый раздел должен быть списком до 40 выводов.",
                )
            })?;
        let mut normalized = Vec::new();
        for item in items {
            v::object(item, &["dimension", "statement", "evidence"])?;
            let dimension = v::required(item, "dimension", 50)?;
            if !DIMENSIONS.contains(&dimension.as_str()) {
                return Err(ApiError::field(
                    "dimension",
                    "Анализируйте только профессиональные требования, без чувствительных признаков.",
                ));
            }
            let evidence = item["evidence"]
                .as_array()
                .filter(|a| a.len() <= 10)
                .ok_or_else(|| {
                    ApiError::field("evidence", "Допускается до 10 ссылок на фрагменты.")
                })?;
            let mut proofs = Vec::new();
            for e in evidence {
                v::object(e, &["source", "path", "quote"])?;
                let source = v::required(e, "source", 20)?;
                if !["cv", "vacancy"].contains(&source.as_str()) {
                    return Err(ApiError::field(
                        "evidence",
                        "Источник цитаты должен быть cv или vacancy.",
                    ));
                }
                proofs.push(json!({"source":source,"path":v::required(e,"path",200)?,"quote":v::optional(e,"quote",1000)?}));
            }
            normalized.push(json!({"dimension":dimension,"statement":v::required(item,"statement",3000)?,"evidence":proofs}));
        }
        output[group] = json!(normalized);
    }
    Ok(output)
}
pub fn normalize(parts: &[&str], input: &Value) -> Result<Value> {
    match parts {
        ["match-runs"] => normalize_selection(input, true),
        ["match-runs", _, "cancel"] => {
            v::object(input, &[])?;
            Ok(json!({}))
        }
        ["match-runs", _, "jobs", _, action]
            if ["claim", "complete", "fail", "retry"].contains(action) =>
        {
            let extra = match *action {
                "claim" => "agent",
                "complete" => "assessment",
                "fail" => "error",
                _ => "",
            };
            let mut fields = vec!["attempt_id", "cv_revision_id"];
            if !extra.is_empty() {
                fields.push(extra);
            }
            if *action == "complete" {
                fields.push("engine");
            }
            v::object(input, &fields)?;
            let mut result = json!({"attempt_id":v::uuid(input,"attempt_id")?,"cv_revision_id":v::uuid(input,"cv_revision_id")?});
            match *action {
                "claim" => {
                    result["agent"] = json!(v::required(input, "agent", 200)?);
                }
                "complete" => {
                    result["engine"] = engine(&input["engine"])?;
                    result["assessment"] = assessment(&input["assessment"])?;
                }
                "fail" => {
                    v::object(&input["error"], &["code", "message"])?;
                    result["error"] = json!({"code":v::required(&input["error"],"code",100)?,"message":v::required(&input["error"],"message",1000)?});
                }
                _ => {}
            }
            Ok(result)
        }
        _ => Err(ApiError::missing()),
    }
}
fn proof_path(source: &str, path: &str) -> bool {
    if source == "vacancy" {
        return [
            "/title",
            "/description",
            "/location",
            "/work_format",
            "/employment_type",
            "/company/name",
        ]
        .contains(&path);
    }
    if [
        "/content",
        "/structured/summary",
        "/structured/skills",
        "/structured/experience",
        "/structured/education",
    ]
    .contains(&path)
    {
        return true;
    }
    let segments: Vec<_> = path.split('/').collect();
    matches!(segments.as_slice(),["","structured","skills"|"education",index] if index.parse::<usize>().is_ok())
        || matches!(segments.as_slice(),["","structured","experience",index,"role"|"details"] if index.parse::<usize>().is_ok())
}
fn validate_evidence(assessment: &Value, cv: &Value, vacancy: &Value) -> Result<()> {
    for group in ["matches", "gaps", "questions", "unknowns"] {
        for item in assessment[group]
            .as_array()
            .ok_or_else(ApiError::internal)?
        {
            let mut cv_quote = false;
            let mut vacancy_quote = false;
            for proof in item["evidence"].as_array().ok_or_else(ApiError::internal)? {
                let source = proof["source"].as_str().ok_or_else(ApiError::internal)?;
                let path = proof["path"].as_str().ok_or_else(ApiError::internal)?;
                if !proof_path(source, path) {
                    return Err(ApiError::field(
                        "evidence",
                        "Цитировать можно только содержимое CV и профессиональные поля вакансии.",
                    ));
                }
                let target = if source == "cv" { cv } else { vacancy };
                let fragment = target.pointer(path).ok_or_else(|| {
                    ApiError::field(
                        "evidence",
                        "Указанный фрагмент отсутствует в зафиксированном входе.",
                    )
                })?;
                if let Some(quote) = proof["quote"].as_str() {
                    if !fragment.as_str().is_some_and(|text| text.contains(quote)) {
                        return Err(ApiError::field(
                            "evidence",
                            "Цитата не найдена в зафиксированном CV или вакансии.",
                        ));
                    }
                    if source == "cv" {
                        cv_quote = true
                    } else {
                        vacancy_quote = true
                    }
                } else if !(fragment.is_null()
                    || fragment.as_str().is_some_and(|s| s.trim().is_empty())
                    || fragment.as_array().is_some_and(Vec::is_empty))
                {
                    return Err(ApiError::field(
                        "evidence",
                        "Ссылка без цитаты допустима только для пустого поля.",
                    ));
                }
            }
            if (group == "matches" && (!cv_quote || !vacancy_quote))
                || (group == "gaps" && !vacancy_quote)
            {
                return Err(ApiError::field(
                    "evidence",
                    "Совпадение требует цитат CV и вакансии; пробел — цитаты требования вакансии.",
                ));
            }
        }
    }
    Ok(())
}
async fn jobs(c: &mut SqliteConnection, run: &str) -> Result<Vec<Value>> {
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT data FROM match_job WHERE run_id=? ORDER BY ingest_seq",
    )
    .bind(run)
    .fetch_all(c)
    .await?;
    rows.iter()
        .map(|r| serde_json::from_str(r).map_err(|_| ApiError::internal()))
        .collect()
}
async fn save_attempt(
    c: &mut SqliteConnection,
    job: &Value,
    state: &str,
    update: &Value,
) -> Result<()> {
    let id = job["attempt_id"].as_str().ok_or_else(ApiError::internal)?;
    let mut attempt = s::get(c, "match_attempt", id).await?;
    attempt["state"] = json!(state);
    attempt["updated_at"] = json!(v::now());
    for (key, value) in update.as_object().ok_or_else(ApiError::internal)? {
        attempt[key] = value.clone();
    }
    s::update(c, "match_attempt", &attempt, &[]).await
}
async fn new_attempt(c: &mut SqliteConnection, job: &Value) -> Result<()> {
    let attempt = s::record(
        json!({"job_id":job["id"],"attempt_number":job["attempt_number"],"state":"pending","agent":null,"engine":null,"assessment":null,"error":null}),
    );
    let mut attempt = attempt;
    attempt["id"] = job["attempt_id"].clone();
    sqlx::query("INSERT INTO match_attempt(id,job_id,attempt_number,data) VALUES(?,?,?,?)")
        .bind(attempt["id"].as_str())
        .bind(job["id"].as_str())
        .bind(job["attempt_number"].as_i64())
        .bind(attempt.to_string())
        .execute(c)
        .await?;
    Ok(())
}
async fn refresh(c: &mut SqliteConnection, run_id: &str) -> Result<Value> {
    let mut run = s::get(c, "match_run", run_id).await?;
    let items = jobs(c, run_id).await?;
    let mut counts =
        json!({"total":items.len(),"pending":0,"running":0,"completed":0,"failed":0,"cancelled":0});
    for job in &items {
        let state = job["state"].as_str().ok_or_else(ApiError::internal)?;
        counts[state] = json!(counts[state].as_u64().ok_or_else(ApiError::internal)? + 1);
    }
    if run["state"] != "cancelled" {
        run["state"] = json!(if counts["completed"] == counts["total"] {
            "completed"
        } else if counts["failed"].as_u64().unwrap_or(0) > 0 {
            "partial"
        } else if counts["running"].as_u64().unwrap_or(0) > 0 {
            "running"
        } else {
            "pending"
        });
    }
    run["counts"] = counts;
    run["updated_at"] = json!(v::now());
    s::update(c, "match_run", &run, &[]).await?;
    Ok(run)
}
async fn create(c: &mut SqliteConnection, input: &Value) -> Result<Value> {
    let (cv, items) = selected(c, input).await?;
    if items.is_empty() {
        return Err(ApiError::field(
            "selection",
            "По выбранным условиям нет вакансий. Пакет не создан.",
        ));
    }
    let run = s::record(
        json!({"cv_id":cv["id"],"cv_revision_id":cv["current_revision_id"],"cv_revision_number":cv["current_revision_number"],
  "cv_name":cv["name"],"selection":input["selection"],"state":"pending","counts":{}}),
    );
    s::insert(
        c,
        "match_run",
        &run,
        &[
            ("cv_id", cv["id"].clone()),
            ("cv_revision_id", cv["current_revision_id"].clone()),
        ],
    )
    .await?;
    for data in items {
        let job = s::record(
            json!({"run_id":run["id"],"vacancy_id":data["id"],"snapshot":snapshot(&data),"attempt_number":1,"attempt_id":v::id(),"state":"pending","assessment":null,"engine":null,"error":null}),
        );
        s::insert(
            c,
            "match_job",
            &job,
            &[
                ("run_id", run["id"].clone()),
                ("vacancy_id", data["id"].clone()),
            ],
        )
        .await?;
        new_attempt(c, &job).await?;
    }
    refresh(c, run["id"].as_str().ok_or_else(ApiError::internal)?).await
}
pub async fn write(c: &mut SqliteConnection, parts: &[&str], input: &Value) -> Result<Value> {
    if parts == ["match-runs"] {
        return create(c, input).await;
    }
    let run_id = parts.get(1).copied().ok_or_else(ApiError::missing)?;
    let mut run = s::get(c, "match_run", run_id).await?;
    if let ["match-runs", _, "cancel"] = parts {
        if run["state"] == "completed" {
            return Err(ApiError::conflict("matching_state_conflict"));
        }
        if run["state"] == "cancelled" {
            return Ok(run);
        }
        for mut job in jobs(c, run_id).await? {
            if ["pending", "running"].contains(&job["state"].as_str().unwrap_or("")) {
                save_attempt(
                    c,
                    &job,
                    "cancelled",
                    &json!({"error":{"code":"cancelled","message":"Пакет отменён пользователем."}}),
                )
                .await?;
                job["state"] = json!("cancelled");
                job["updated_at"] = json!(v::now());
                s::update(c, "match_job", &job, &[]).await?;
            }
        }
        run["state"] = json!("cancelled");
        s::update(c, "match_run", &run, &[]).await?;
        return refresh(c, run_id).await;
    }
    if ["cancelled", "completed"].contains(&run["state"].as_str().unwrap_or("")) {
        return Err(ApiError::conflict("matching_state_conflict"));
    }
    let ["match-runs", _, "jobs", job_id, action] = parts else {
        return Err(ApiError::missing());
    };
    let mut job = s::get(c, "match_job", job_id).await?;
    if job["run_id"] != run_id {
        return Err(ApiError::missing());
    }
    if input["cv_revision_id"] != run["cv_revision_id"] || input["attempt_id"] != job["attempt_id"]
    {
        return Err(ApiError::conflict("matching_input_conflict"));
    }
    let state = job["state"]
        .as_str()
        .ok_or_else(ApiError::internal)?
        .to_owned();
    match *action {
        "claim" => {
            if state != "pending" {
                return Err(ApiError::conflict("matching_state_conflict"));
            }
            save_attempt(c, &job, "running", &json!({"agent":input["agent"]})).await?;
            job["state"] = json!("running");
        }
        "complete" => {
            if state != "running" {
                return Err(ApiError::conflict("matching_state_conflict"));
            }
            let revision = s::get(
                c,
                "cv_revision",
                run["cv_revision_id"]
                    .as_str()
                    .ok_or_else(ApiError::internal)?,
            )
            .await?;
            let attempt = s::get(
                c,
                "match_attempt",
                job["attempt_id"].as_str().ok_or_else(ApiError::internal)?,
            )
            .await?;
            if attempt["agent"] != input["engine"]["agent"] {
                return Err(ApiError::conflict("matching_agent_conflict"));
            }
            validate_evidence(&input["assessment"], &revision, &job["snapshot"])?;
            save_attempt(
                c,
                &job,
                "completed",
                &json!({"engine":input["engine"],"assessment":input["assessment"]}),
            )
            .await?;
            job["state"] = json!("completed");
            job["assessment"] = input["assessment"].clone();
            job["engine"] = input["engine"].clone();
        }
        "fail" => {
            if state != "running" {
                return Err(ApiError::conflict("matching_state_conflict"));
            }
            save_attempt(c, &job, "failed", &json!({"error":input["error"]})).await?;
            job["state"] = json!("failed");
            job["error"] = input["error"].clone();
        }
        "retry" => {
            if !["failed", "running"].contains(&state.as_str()) {
                return Err(ApiError::conflict("matching_state_conflict"));
            }
            let number = job["attempt_number"]
                .as_i64()
                .ok_or_else(ApiError::internal)?;
            if number >= 100 {
                return Err(ApiError::field(
                    "attempt",
                    "Достигнут предел 100 попыток. Создайте новый пакет.",
                ));
            }
            if state == "running" {
                save_attempt(c,&job,"failed",&json!({"error":{"code":"interrupted","message":"Пользователь переочередил задание; предыдущая попытка отозвана."}})).await?;
            }
            job["attempt_number"] = json!(number + 1);
            job["attempt_id"] = json!(v::id());
            job["state"] = json!("pending");
            job["error"] = Value::Null;
            new_attempt(c, &job).await?;
        }
        _ => return Err(ApiError::missing()),
    }
    job["updated_at"] = json!(v::now());
    s::update(c, "match_job", &job, &[]).await?;
    refresh(c, run_id).await?;
    Ok(job)
}
pub async fn hydrate_run(c: &mut SqliteConnection, mut run: Value) -> Result<Value> {
    let cv = s::get(
        c,
        "cv",
        run["cv_id"].as_str().ok_or_else(ApiError::internal)?,
    )
    .await?;
    let cv_stale = cv["current_revision_id"] != run["cv_revision_id"];
    let mut stale_jobs = 0;
    for job in jobs(c, run["id"].as_str().ok_or_else(ApiError::internal)?).await? {
        if current_snapshot(
            c,
            job["vacancy_id"].as_str().ok_or_else(ApiError::internal)?,
        )
        .await?
            != job["snapshot"]
        {
            stale_jobs += 1;
        }
    }
    run["cv_stale"] = json!(cv_stale);
    run["stale_vacancies"] = json!(stale_jobs);
    run["stale"] = json!(cv_stale || stale_jobs > 0);
    Ok(run)
}
pub async fn read(c: &mut SqliteConnection, parts: &[&str]) -> Result<Value> {
    let run_id = parts.get(1).copied().ok_or_else(ApiError::missing)?;
    let stored = s::get(c, "match_run", run_id).await?;
    let run = hydrate_run(c, stored).await?;
    match parts {
        ["match-runs", _, "inputs"] => {
            let revision = s::get(
                c,
                "cv_revision",
                run["cv_revision_id"]
                    .as_str()
                    .ok_or_else(ApiError::internal)?,
            )
            .await?;
            Ok(
                json!({"data":{"schema_version":1,"run":run,"cv_revision":revision,"jobs":jobs(c,run_id).await?,
    "policy":{"explicit_agent_only":true,"no_sensitive_dimensions":true,"no_outreach":true,"no_status_changes":true,"evidence_must_quote_pinned_inputs":true}}}),
            )
        }
        ["match-runs", _, "jobs"] => {
            let mut items = jobs(c, run_id).await?;
            for job in &mut items {
                job["vacancy_title"] = job["snapshot"]["title"].clone();
                job["company_name"] = job["snapshot"]["company"]["name"].clone();
                job["stale"] = json!(
                    run["cv_stale"] == true
                        || current_snapshot(
                            c,
                            job["vacancy_id"].as_str().ok_or_else(ApiError::internal)?
                        )
                        .await?
                            != job["snapshot"]
                );
                if let Some(o) = job.as_object_mut() {
                    o.remove("snapshot");
                    o.remove("assessment");
                }
            }
            Ok(json!({"items":items,"next_cursor":null}))
        }
        ["match-runs", _, "jobs", job_id] | ["match-runs", _, "jobs", job_id, "attempts"] => {
            let mut job = s::get(c, "match_job", job_id).await?;
            if job["run_id"] != run_id {
                return Err(ApiError::missing());
            }
            if parts.len() == 5 {
                let rows = sqlx::query_scalar::<_, String>(
                    "SELECT data FROM match_attempt WHERE job_id=? ORDER BY attempt_number DESC",
                )
                .bind(job_id)
                .fetch_all(c)
                .await?;
                let items: Vec<Value> = rows
                    .iter()
                    .map(|r| serde_json::from_str(r).map_err(|_| ApiError::internal()))
                    .collect::<Result<_>>()?;
                return Ok(json!({"items":items,"next_cursor":null}));
            }
            let revision = s::get(
                c,
                "cv_revision",
                run["cv_revision_id"]
                    .as_str()
                    .ok_or_else(ApiError::internal)?,
            )
            .await?;
            job["stale"] = json!(
                run["cv_stale"] == true
                    || current_snapshot(
                        c,
                        job["vacancy_id"].as_str().ok_or_else(ApiError::internal)?
                    )
                    .await?
                        != job["snapshot"]
            );
            Ok(json!({"data":job,"run":run,"cv_revision":revision}))
        }
        _ => Err(ApiError::missing()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sensitive_and_unrelated_paths_are_not_evidence() {
        assert!(!proof_path("cv", "/age"));
        assert!(!proof_path("vacancy", "/notes"));
        assert!(!proof_path("vacancy", "/status"));
        assert!(proof_path("cv", "/structured/experience/0/details"));
        assert!(proof_path("vacancy", "/description"));
    }
    #[test]
    fn fabricated_quotes_and_missing_cross_input_evidence_are_rejected() {
        let a = json!({"matches":[{"evidence":[{"source":"cv","path":"/content","quote":"Rust"},{"source":"vacancy","path":"/title","quote":"Rust"}]}],"gaps":[],"questions":[],"unknowns":[]});
        assert!(
            validate_evidence(
                &a,
                &json!({"content":"Rust APIs"}),
                &json!({"title":"Rust Engineer"})
            )
            .is_ok()
        );
        assert!(
            validate_evidence(
                &a,
                &json!({"content":"Python"}),
                &json!({"title":"Rust Engineer"})
            )
            .is_err()
        );
        let missing = json!({"matches":[{"evidence":[{"source":"cv","path":"/content","quote":"Rust"}]}],"gaps":[],"questions":[],"unknowns":[]});
        assert!(
            validate_evidence(
                &missing,
                &json!({"content":"Rust"}),
                &json!({"title":"Rust"})
            )
            .is_err()
        );
    }
}
