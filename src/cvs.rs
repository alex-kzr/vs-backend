//! Versioned local CVs. Import preview is pure; confirmed writes never run matching.
use crate::{
    error::{ApiError, Result},
    storage as s, validation as v,
};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};

pub fn hh_url(value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let raw = value
        .as_str()
        .ok_or_else(|| ApiError::field("hh_url", "Укажите ссылку на конкретное резюме HH."))?;
    let u = url::Url::parse(raw)
        .map_err(|_| ApiError::field("hh_url", "Некорректная ссылка на резюме HH."))?;
    let segments: Vec<_> = u.path().trim_end_matches('/').split('/').collect();
    if u.scheme() != "https"
        || !matches!(u.host_str(), Some("hh.ru" | "www.hh.ru"))
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
        || segments.len() != 3
        || segments[1] != "resume"
        || segments[2].is_empty()
        || segments[2].len() > 128
        || !segments[2]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        || raw.chars().any(char::is_whitespace)
    {
        return Err(ApiError::field(
            "hh_url",
            "Используйте HTTPS ссылку hh.ru/resume/ID без логина и пароля.",
        ));
    }
    Ok(json!(format!("https://hh.ru/resume/{}", segments[2])))
}
fn structured(input: &Value) -> Result<Value> {
    let empty = json!({});
    let input = if input.is_null() { &empty } else { input };
    v::object(input, &["summary", "skills", "experience", "education"])?;
    let mut skills = Vec::new();
    if let Some(array) = input.get("skills") {
        for skill in array
            .as_array()
            .filter(|a| a.len() <= 100)
            .ok_or_else(|| ApiError::field("structured", "Допускается до 100 навыков."))?
        {
            let text = v::required(&json!({"skill":skill}), "skill", 200)?;
            if skills.contains(&text) {
                return Err(ApiError::field(
                    "structured",
                    "Навыки не должны повторяться.",
                ));
            }
            skills.push(text);
        }
    }
    let mut experience = Vec::new();
    if let Some(array) = input.get("experience") {
        for item in array
            .as_array()
            .filter(|a| a.len() <= 50)
            .ok_or_else(|| ApiError::field("structured", "Допускается до 50 записей опыта."))?
        {
            v::object(item, &["role", "details"])?;
            experience.push(json!({"role":v::required(item,"role",300)?,"details":v::optional(item,"details",20000)?}));
        }
    }
    let mut education = Vec::new();
    if let Some(array) = input.get("education") {
        for item in array.as_array().filter(|a| a.len() <= 50).ok_or_else(|| {
            ApiError::field("structured", "Допускается до 50 записей образования.")
        })? {
            education.push(v::required(&json!({"education":item}), "education", 3000)?);
        }
    }
    Ok(
        json!({"summary":v::optional(input,"summary",10000)?,"skills":skills,"experience":experience,"education":education}),
    )
}
pub fn normalize(input: &Value, confirmed: bool, revision: bool) -> Result<Value> {
    let mut allowed = vec![
        "name",
        "hh_url",
        "source_code",
        "source_url",
        "external_id",
        "content",
        "structured",
        "source",
    ];
    if confirmed {
        allowed.push("confirmed");
    }
    if revision {
        allowed.push("expected_revision_id");
    }
    v::object(input, &allowed)?;
    if confirmed && !v::boolean(input, "confirmed", false)? {
        return Err(ApiError::field(
            "confirmed",
            "Подтвердите просмотр содержимого CV.",
        ));
    }
    let content = input["content"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 200_000 && !s.contains('\0'))
        .ok_or_else(|| {
            ApiError::field(
                "content",
                "CV должно содержать непустой текст, не более 200000 символов.",
            )
        })?;
    let legacy = hh_url(input.get("hh_url").unwrap_or(&Value::Null))?;
    let code = source_code(
        input
            .get("source_code")
            .unwrap_or(&json!(if legacy.is_null() { "manual" } else { "hh.ru" })),
    )?;
    if !legacy.is_null() && code != "hh.ru" {
        return Err(ApiError::field(
            "source_code",
            "Для ссылки HH выберите источник hh.ru.",
        ));
    }
    let url = source_url(&code, input.get("source_url").unwrap_or(&legacy))?;
    if !legacy.is_null() && legacy != url {
        return Err(ApiError::field(
            "source_url",
            "Ссылки на CV должны обозначать одно резюме.",
        ));
    }
    let link = if code == "hh.ru" {
        url.clone()
    } else {
        Value::Null
    };
    let mut external = v::optional(input, "external_id", 300)?;
    if let Some(link) = link.as_str() {
        let resume_id = link
            .strip_prefix("https://hh.ru/resume/")
            .ok_or_else(ApiError::internal)?;
        if !external.is_null() && external != resume_id {
            return Err(ApiError::field(
                "external_id",
                "ID резюме должен совпадать с его ссылкой HH.",
            ));
        }
        external = json!(resume_id);
    }
    let source = &input["source"];
    v::object(
        source,
        &["kind", "observed_at", "url", "file_name", "agent"],
    )?;
    let kind = v::required(source, "kind", 30)?;
    if !["manual_text", "manual_file", "manual_edit", "hh_browser"].contains(&kind.as_str()) {
        return Err(ApiError::field("source", "Неизвестный способ импорта CV."));
    }
    let source_url = source_url(&code, source.get("url").unwrap_or(&Value::Null))?;
    let file = v::optional(source, "file_name", 255)?;
    if file.as_str().is_some_and(|s| s.contains(['/', '\\'])) {
        return Err(ApiError::field("source", "Укажите только имя файла."));
    }
    let agent = v::optional(source, "agent", 200)?;
    if kind == "hh_browser" && (link.is_null() || source_url != link || agent.is_null()) {
        return Err(ApiError::field(
            "source",
            "Браузерный импорт требует URL того же CV и имени агента.",
        ));
    }
    if kind == "manual_file" && file.is_null() {
        return Err(ApiError::field(
            "source",
            "Укажите имя импортированного файла.",
        ));
    }
    let mut normalized = json!({"name":v::required(input,"name",300)?,"hh_url":link,"source_code":code,"source_url":url,"external_id":external,"content":content,"structured":structured(&input["structured"])?,
  "source":{"kind":kind,"observed_at":v::time(source,"observed_at")?,"url":source_url,"file_name":file,"agent":agent}});
    if revision {
        normalized["expected_revision_id"] = json!(v::uuid(input, "expected_revision_id")?);
    }
    Ok(normalized)
}
pub async fn save(c: &mut SqliteConnection, parent: Option<&str>, input: &Value) -> Result<Value> {
    let mut cv = if let Some(id) = parent {
        require_active(c, id).await?;
        let old = s::get(c, "cv", id).await?;
        let identity = metadata(c, old.clone()).await?;
        if ["source_code", "source_url", "external_id"]
            .iter()
            .any(|key| identity[*key] != input[*key])
        {
            return Err(ApiError::field(
                "source_code",
                "Источник и внешнюю идентичность существующего CV менять нельзя. Импортируйте отдельное CV из другого источника.",
            ));
        }
        if old["current_revision_id"] != input["expected_revision_id"] {
            return Err(ApiError::conflict("cv_revision_conflict"));
        }
        old
    } else {
        s::record(json!({}))
    };
    let code = input["source_code"]
        .as_str()
        .ok_or_else(ApiError::internal)?;
    let owner = sqlx::query("SELECT id,deleted_at FROM cv WHERE source_code=? AND ((? IS NOT NULL AND source_url=?) OR (? IS NOT NULL AND external_id=?))")
        .bind(code).bind(input["source_url"].as_str()).bind(input["source_url"].as_str())
        .bind(input["external_id"].as_str()).bind(input["external_id"].as_str())
        .fetch_all(&mut *c).await?;
    for row in owner {
        let owner_id: String = row.get("id");
        if Some(owner_id.as_str()) != parent {
            let deleted: Option<String> = row.get("deleted_at");
            let code = if deleted.is_some() {
                "cv_deleted_duplicate_source"
            } else if code == "hh.ru" {
                "cv_duplicate_url"
            } else {
                "cv_duplicate_source"
            };
            return Err(ApiError::conflict(code));
        }
    }
    register_source(c, code).await?;
    let number = cv["current_revision_number"].as_i64().unwrap_or(0) + 1;
    let revision = s::record(
        json!({"cv_id":cv["id"],"revision_number":number,"name":input["name"],"hh_url":input["hh_url"],"source_code":input["source_code"],"source_url":input["source_url"],"external_id":input["external_id"],
  "content":input["content"],"structured":input["structured"],"source":input["source"],"imported_at":v::now()}),
    );
    cv["name"] = input["name"].clone();
    cv["hh_url"] = input["hh_url"].clone();
    cv["current_revision_id"] = revision["id"].clone();
    cv["current_revision_number"] = json!(number);
    cv["imported_at"] = revision["imported_at"].clone();
    cv["updated_at"] = json!(v::now());
    let columns = [
        ("hh_url", cv["hh_url"].clone()),
        ("source_code", input["source_code"].clone()),
        ("source_url", input["source_url"].clone()),
        ("external_id", input["external_id"].clone()),
    ];
    if parent.is_some() {
        s::update(c, "cv", &cv, &columns).await?;
    } else {
        s::insert(c, "cv", &cv, &columns).await?;
    }
    sqlx::query("INSERT INTO cv_revision(id,cv_id,revision_number,data) VALUES(?,?,?,?)")
        .bind(revision["id"].as_str())
        .bind(cv["id"].as_str())
        .bind(number)
        .bind(revision.to_string())
        .execute(&mut *c)
        .await?;
    metadata(c, cv).await
}
pub async fn detail(c: &mut SqliteConnection, mut cv: Value) -> Result<Value> {
    cv["revision"] = s::get(
        c,
        "cv_revision",
        cv["current_revision_id"]
            .as_str()
            .ok_or_else(ApiError::internal)?,
    )
    .await?;
    Ok(cv)
}
pub async fn read(c: &mut SqliteConnection, parts: &[&str]) -> Result<Value> {
    match parts {
        ["cvs", id, "revisions", revision] => {
            let r = s::get(c, "cv_revision", revision).await?;
            if r["cv_id"] != *id {
                return Err(ApiError::missing());
            }
            Ok(json!({"data":r}))
        }
        _ => Err(ApiError::missing()),
    }
}
pub fn source_code(value: &Value) -> Result<String> {
    let raw = value.as_str().ok_or_else(|| {
        ApiError::field("source_code", "Выберите источник CV или укажите его домен.")
    })?;
    let code = raw.trim().to_ascii_lowercase();
    if code != "manual"
        && (code.len() > 253
            || !code.contains('.')
            || code.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }))
    {
        return Err(ApiError::field(
            "source_code",
            "Укажите домен источника без протокола, пути и порта.",
        ));
    }
    Ok(code)
}
fn source_url(code: &str, value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    if code == "hh.ru" {
        return hh_url(value);
    }
    let raw = value
        .as_str()
        .ok_or_else(|| ApiError::field("source_url", "Укажите HTTPS ссылку на CV."))?;
    let mut u = url::Url::parse(raw)
        .map_err(|_| ApiError::field("source_url", "Некорректная ссылка источника."))?;
    if raw.chars().count() > 2048
        || code == "manual"
        || u.scheme() != "https"
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
        || raw.chars().any(char::is_whitespace)
        || !(u.host_str() == Some(code) || u.host_str() == Some(format!("www.{code}").as_str()))
    {
        return Err(ApiError::field(
            "source_url",
            "HTTPS ссылка должна принадлежать выбранному источнику; логин, пароль и порт недопустимы.",
        ));
    }
    u.set_host(Some(code)).map_err(|_| ApiError::internal())?;
    u.set_fragment(None);
    // Unknown sources retain query parameters: they may identify the actual CV.
    Ok(json!(u.as_str()))
}
async fn register_source(c: &mut SqliteConnection, code: &str) -> Result<()> {
    if sqlx::query_scalar::<_, i64>("SELECT 1 FROM cv_source WHERE code=?")
        .bind(code)
        .fetch_optional(&mut *c)
        .await?
        .is_some()
    {
        return Ok(());
    }
    let data = s::record(json!({"code":code,"label":if code=="manual" {"Вручную"} else {code}}));
    sqlx::query("INSERT INTO cv_source(id,code,data) VALUES(?,?,?) ON CONFLICT(code) DO NOTHING")
        .bind(data["id"].as_str())
        .bind(code)
        .bind(data.to_string())
        .execute(c)
        .await?;
    Ok(())
}
pub async fn metadata(c: &mut SqliteConnection, mut cv: Value) -> Result<Value> {
    let row = sqlx::query(
        "SELECT deleted_at,lifecycle_version,source_code,source_url,external_id FROM cv WHERE id=?",
    )
    .bind(cv["id"].as_str())
    .fetch_one(c)
    .await?;
    cv["deleted_at"] = json!(row.get::<Option<String>, _>("deleted_at"));
    cv["lifecycle_version"] = json!(row.get::<i64, _>("lifecycle_version"));
    for key in ["source_code", "source_url", "external_id"] {
        cv[key] = json!(row.get::<Option<String>, _>(key));
    }
    Ok(cv)
}
pub async fn require_active(c: &mut SqliteConnection, id: &str) -> Result<()> {
    let deleted = sqlx::query_scalar::<_, Option<String>>("SELECT deleted_at FROM cv WHERE id=?")
        .bind(id)
        .fetch_optional(c)
        .await?
        .ok_or_else(ApiError::missing)?;
    if deleted.is_some() {
        return Err(ApiError::conflict("cv_deleted"));
    }
    Ok(())
}
pub fn normalize_lifecycle(input: &Value) -> Result<Value> {
    v::object(
        input,
        &[
            "confirmed",
            "expected_revision_id",
            "expected_lifecycle_version",
        ],
    )?;
    if !v::boolean(input, "confirmed", false)? {
        return Err(ApiError::field("confirmed", "Подтвердите действие с CV."));
    }
    let version = input["expected_lifecycle_version"]
        .as_i64()
        .filter(|v| *v >= 0)
        .ok_or_else(|| {
            ApiError::field(
                "expected_lifecycle_version",
                "Укажите текущую версию состояния CV.",
            )
        })?;
    Ok(
        json!({"expected_revision_id":v::uuid(input,"expected_revision_id")?,"expected_lifecycle_version":version}),
    )
}
pub async fn change_lifecycle(
    c: &mut SqliteConnection,
    id: &str,
    delete: bool,
    input: &Value,
) -> Result<Value> {
    let raw = s::get(c, "cv", id).await?;
    let cv = metadata(c, raw.clone()).await?;
    if cv["lifecycle_version"] != input["expected_lifecycle_version"] {
        return Err(ApiError::conflict("cv_lifecycle_conflict"));
    }
    if cv["current_revision_id"] != input["expected_revision_id"] {
        return Err(ApiError::conflict("cv_revision_conflict"));
    }
    if delete == cv["deleted_at"].is_null() {
        sqlx::query("UPDATE cv SET deleted_at=?,lifecycle_version=lifecycle_version+1 WHERE id=?")
            .bind(if delete { Some(v::now()) } else { None })
            .bind(id)
            .execute(&mut *c)
            .await?;
    }
    metadata(c, raw).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canonical_hh_urls_reject_foreign_or_credential_links() {
        assert_eq!(
            hh_url(&json!("https://www.hh.ru/resume/test_123/?from=share#top")).unwrap(),
            json!("https://hh.ru/resume/test_123")
        );
        for url in [
            "http://hh.ru/resume/x",
            "https://hh.ru.evil.test/resume/x",
            "https://user:pass@hh.ru/resume/x",
            "https://hh.ru/applicant/resumes",
            "https://hh.ru/resume/x/y",
            "https://hh.ru/resume/",
            "https://hh.ru/resume/x%2Fy",
        ] {
            assert!(hh_url(&json!(url)).is_err(), "{url}");
        }
    }
    #[test]
    fn sensitive_structured_dimensions_and_unknown_source_fields_are_rejected() {
        assert!(structured(&json!({"age":30})).is_err());
        assert!(structured(&json!({"skills":["Rust","Rust"]})).is_err());
    }
}
