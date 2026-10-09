use crate::{
    error::{ApiError, Result},
    validation as v,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const WORK_FORMATS: &[&str] = &["ON_SITE", "REMOTE", "HYBRID", "FIELD_WORK"];
pub const EXPERIENCES: &[&str] = &["noExperience", "between1And3", "between3And6", "moreThan6"];
pub const EMPLOYMENT_FORMS: &[&str] = &["FULL", "PART", "PROJECT", "FLY_IN_FLY_OUT"];
pub const MAX_RESULTS: u64 = 200;

pub fn normalize(input: &Value) -> Result<Value> {
    v::object(
        input,
        &[
            "text",
            "areas",
            "experience",
            "work_formats",
            "employment_forms",
            "period_days",
            "per_page",
        ],
    )?;
    let text = v::optional(input, "text", 1000)?;
    let areas = input.get("areas").cloned().unwrap_or_else(|| json!([]));
    let rows = areas
        .as_array()
        .filter(|a| a.len() <= 10)
        .ok_or_else(|| ApiError::field("hh.areas", "Choose at most 10 regions."))?;
    let mut normalized_areas = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for area in rows {
        v::object(area, &["id", "name"])?;
        let id = v::required(area, "id", 20)?;
        if !id.chars().all(|c| c.is_ascii_digit()) || !seen.insert(id.to_owned()) {
            return Err(ApiError::field(
                "hh.areas",
                "Use unique numeric HH region IDs.",
            ));
        }
        normalized_areas.push(json!({"id":id,"name":v::required(area,"name",300)?}));
    }
    let per_page = input.get("per_page").and_then(Value::as_u64).unwrap_or(20);
    if !(1..=50).contains(&per_page) || input.get("per_page").is_some_and(|x| x.as_u64().is_none())
    {
        return Err(ApiError::field(
            "hh.per_page",
            "Use an integer from 1 to 50.",
        ));
    }
    let period = input.get("period_days").cloned().unwrap_or(Value::Null);
    if !period.is_null() && !period.as_u64().is_some_and(|x| (1..=30).contains(&x)) {
        return Err(ApiError::field(
            "hh.period_days",
            "Use 1-30 days or leave empty.",
        ));
    }
    Ok(json!({"text":text,"areas":normalized_areas,
        "experience":v::array(input,"experience",Some(EXPERIENCES))?,
        "work_formats":v::array(input,"work_formats",Some(WORK_FORMATS))?,
        "employment_forms":v::array(input,"employment_forms",Some(EMPLOYMENT_FORMS))?,
        "period_days":period,"per_page":per_page}))
}

pub struct PreviewQuery {
    pub adapter: String,
    pub page: u64,
    pub request_id: Uuid,
}
pub fn preview_query(raw: &str) -> Result<PreviewQuery> {
    let args = arguments(raw, &["adapter", "page", "request_id"])?;
    let adapter = args
        .get("adapter")
        .cloned()
        .unwrap_or_else(|| "fixture-v1".into());
    if !["fixture-v1", "hh"].contains(&adapter.as_str()) {
        return Err(invalid());
    }
    let page = args
        .get("page")
        .map(|x| x.parse::<u64>())
        .transpose()
        .map_err(|_| invalid())?
        .unwrap_or(0);
    if adapter == "fixture-v1" && page != 0 {
        return Err(invalid());
    }
    let request_id = args
        .get("request_id")
        .map(|x| Uuid::parse_str(x))
        .transpose()
        .map_err(|_| invalid())?
        .unwrap_or_else(Uuid::new_v4);
    Ok(PreviewQuery {
        adapter,
        page,
        request_id,
    })
}
pub fn arguments(raw: &str, allowed: &[&str]) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (k, value) in url::form_urlencoded::parse(raw.as_bytes()) {
        if !allowed.contains(&k.as_ref())
            || out.insert(k.into_owned(), value.into_owned()).is_some()
        {
            return Err(invalid());
        }
    }
    Ok(out)
}
pub fn invalid() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "invalid_query",
        "Invalid HH preview query. Start at the first page.",
    )
}
pub fn parameters(configuration: &Value, page: u64) -> Result<(Value, Vec<(String, String)>)> {
    let hh = normalize(configuration["criteria"].get("hh").ok_or_else(|| {
        ApiError::field("hh", "Edit this configuration and add an HH search query.")
    })?)?;
    let text = hh["text"]
        .as_str()
        .filter(|x| !x.trim().is_empty())
        .ok_or_else(|| ApiError::field("hh.text", "Enter your own HH query before searching."))?;
    let size = hh["per_page"].as_u64().ok_or_else(ApiError::internal)?;
    if page
        .checked_add(1)
        .and_then(|p| p.checked_mul(size))
        .is_none_or(|n| n > MAX_RESULTS)
    {
        return Err(invalid());
    }
    let mut out = vec![
        ("text".into(), text.into()),
        ("page".into(), page.to_string()),
        ("per_page".into(), size.to_string()),
        ("host".into(), "hh.ru".into()),
        ("order_by".into(), "publication_time".into()),
        ("no_magic".into(), "true".into()),
    ];
    for area in hh["areas"].as_array().ok_or_else(ApiError::internal)? {
        out.push((
            "area".into(),
            area["id"].as_str().ok_or_else(ApiError::internal)?.into(),
        ));
    }
    for (local, remote) in [
        ("experience", "experience"),
        ("work_formats", "work_format"),
        ("employment_forms", "employment_form"),
    ] {
        for value in hh[local].as_array().ok_or_else(ApiError::internal)? {
            out.push((
                remote.into(),
                value.as_str().ok_or_else(ApiError::internal)?.into(),
            ));
        }
    }
    if let Some(period) = hh["period_days"].as_u64() {
        out.push(("period".into(), period.to_string()));
    }
    Ok((hh, out))
}
