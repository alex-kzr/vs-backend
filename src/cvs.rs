//! Versioned local CVs. Import preview is pure; confirmed writes never run matching.
use crate::{
    error::{ApiError, Result},
    storage as s, validation as v,
};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

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
    let mut allowed = vec!["name", "hh_url", "content", "structured", "source"];
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
    let link = hh_url(input.get("hh_url").unwrap_or(&Value::Null))?;
    let source = &input["source"];
    v::object(
        source,
        &["kind", "observed_at", "url", "file_name", "agent"],
    )?;
    let kind = v::required(source, "kind", 30)?;
    if !["manual_text", "manual_file", "manual_edit", "hh_browser"].contains(&kind.as_str()) {
        return Err(ApiError::field("source", "Неизвестный способ импорта CV."));
    }
    let source_url = hh_url(source.get("url").unwrap_or(&Value::Null))?;
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
    let mut normalized = json!({"name":v::required(input,"name",300)?,"hh_url":link,"content":content,"structured":structured(&input["structured"])?,
  "source":{"kind":kind,"observed_at":v::time(source,"observed_at")?,"url":source_url,"file_name":file,"agent":agent}});
    if revision {
        normalized["expected_revision_id"] = json!(v::uuid(input, "expected_revision_id")?);
    }
    Ok(normalized)
}
pub async fn save(c: &mut SqliteConnection, parent: Option<&str>, input: &Value) -> Result<Value> {
    let mut cv = if let Some(id) = parent {
        let old = s::get(c, "cv", id).await?;
        if old["current_revision_id"] != input["expected_revision_id"] {
            return Err(ApiError::conflict("cv_revision_conflict"));
        }
        old
    } else {
        s::record(json!({}))
    };
    if let Some(link) = input["hh_url"].as_str() {
        let owner = sqlx::query_scalar::<_, String>("SELECT id FROM cv WHERE hh_url=?")
            .bind(link)
            .fetch_optional(&mut *c)
            .await?;
        if owner.as_deref().is_some_and(|id| Some(id) != parent) {
            return Err(ApiError::conflict("cv_duplicate_url"));
        }
    }
    let number = cv["current_revision_number"].as_i64().unwrap_or(0) + 1;
    let revision = s::record(
        json!({"cv_id":cv["id"],"revision_number":number,"name":input["name"],"hh_url":input["hh_url"],
  "content":input["content"],"structured":input["structured"],"source":input["source"],"imported_at":v::now()}),
    );
    cv["name"] = input["name"].clone();
    cv["hh_url"] = input["hh_url"].clone();
    cv["current_revision_id"] = revision["id"].clone();
    cv["current_revision_number"] = json!(number);
    cv["imported_at"] = revision["imported_at"].clone();
    cv["updated_at"] = json!(v::now());
    if parent.is_some() {
        s::update(c, "cv", &cv, &[("hh_url", cv["hh_url"].clone())]).await?;
    } else {
        s::insert(c, "cv", &cv, &[("hh_url", cv["hh_url"].clone())]).await?;
    }
    sqlx::query("INSERT INTO cv_revision(id,cv_id,revision_number,data) VALUES(?,?,?,?)")
        .bind(revision["id"].as_str())
        .bind(cv["id"].as_str())
        .bind(number)
        .bind(revision.to_string())
        .execute(c)
        .await?;
    Ok(cv)
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
