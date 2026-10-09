use crate::error::{ApiError, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::HashSet;
use uuid::Uuid;

pub const STATUSES: &[&str] = &["new", "saved", "contacted", "rejected", "archived"];
pub const FORMATS: &[&str] = &["remote", "hybrid", "onsite"];
pub const EMPLOYMENT: &[&str] = &[
    "full_time",
    "part_time",
    "contract",
    "internship",
    "temporary",
];
pub fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
pub fn id() -> String {
    Uuid::new_v4().to_string()
}
pub fn object(v: &Value, allowed: &[&str]) -> Result<()> {
    let obj = v
        .as_object()
        .ok_or_else(|| ApiError::field("body", "Expected an object."))?;
    if let Some(k) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(ApiError::field(k, "Unknown field."));
    }
    Ok(())
}
pub fn required(v: &Value, k: &str, max: usize) -> Result<String> {
    let s = v[k]
        .as_str()
        .ok_or_else(|| ApiError::field(k, "Required text."))?
        .trim();
    if s.is_empty() || s.chars().count() > max {
        return Err(ApiError::field(
            k,
            "Text is empty or exceeds the allowed length.",
        ));
    }
    Ok(s.into())
}
pub fn optional(v: &Value, k: &str, max: usize) -> Result<Value> {
    if v.get(k).is_none_or(Value::is_null) {
        return Ok(Value::Null);
    }
    Ok(Value::String(required(v, k, max)?))
}
pub fn uuid(v: &Value, k: &str) -> Result<String> {
    let s = required(v, k, 100)?;
    Ok(Uuid::parse_str(&s)
        .map_err(|_| ApiError::field(k, "Expected a UUID."))?
        .to_string())
}
pub fn optional_uuid(v: &Value, k: &str) -> Result<Value> {
    if v.get(k).is_none_or(Value::is_null) {
        Ok(Value::Null)
    } else {
        Ok(json!(uuid(v, k)?))
    }
}
pub fn boolean(v: &Value, k: &str, default: bool) -> Result<bool> {
    v.get(k).map_or(Ok(default), |v| {
        v.as_bool()
            .ok_or_else(|| ApiError::field(k, "Expected a boolean."))
    })
}
pub fn enum_optional(v: &Value, k: &str, choices: &[&str]) -> Result<Value> {
    let value = optional(v, k, 100)?;
    if let Some(s) = value.as_str()
        && !choices.contains(&s)
    {
        return Err(ApiError::field(k, "Unsupported value."));
    }
    Ok(value)
}
pub fn time(v: &Value, k: &str) -> Result<String> {
    let s = required(v, k, 100)?;
    let t = DateTime::parse_from_rfc3339(&s)
        .map_err(|_| ApiError::field(k, "Use RFC3339 with a timezone offset."))?;
    if t.timestamp_subsec_nanos() % 1_000_000 != 0 {
        return Err(ApiError::field(
            k,
            "Precision beyond milliseconds is unsupported.",
        ));
    }
    Ok(t.with_timezone(&Utc)
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}
pub fn url(v: &Value, k: &str) -> Result<Value> {
    let value = optional(v, k, 2048)?;
    if let Some(s) = value.as_str() {
        normalize_url(s)
            .map_err(|_| ApiError::field(k, "Use an absolute HTTP(S) URL without credentials."))?;
    }
    Ok(value)
}
pub fn normalize_url(s: &str) -> Result<String> {
    let u = url::Url::parse(s).map_err(|_| ApiError::field("url", "Invalid URL."))?;
    if !matches!(u.scheme(), "http" | "https")
        || !u.username().is_empty()
        || u.password().is_some()
        || u.host_str().is_none()
        || s.chars().any(char::is_whitespace)
    {
        return Err(ApiError::field("url", "Invalid URL."));
    }
    // Keep path/query/fragment bytes unchanged; parsing is only for validation and authority.
    let (_, tail) = s
        .split_once("://")
        .ok_or_else(|| ApiError::field("url", "Absolute URL required."))?;
    let end = tail.find(['/', '?', '#']).unwrap_or(tail.len());
    let authority = &tail[..end];
    if authority.contains('@') {
        return Err(ApiError::field("url", "Credentials are unsupported."));
    }
    let mut authority = authority.to_lowercase();
    let default = if u.scheme() == "http" { ":80" } else { ":443" };
    if authority.ends_with(default) {
        authority.truncate(authority.len() - default.len());
    }
    Ok(format!("{}://{}{}", u.scheme(), authority, &tail[end..]))
}
pub fn email(v: &Value) -> Result<(Value, Value)> {
    let e = optional(v, "email", 320)?;
    if let Some(s) = e.as_str() {
        let (local, domain) = s
            .rsplit_once('@')
            .ok_or_else(|| ApiError::field("email", "Invalid email."))?;
        if local.is_empty()
            || local.contains('@')
            || !domain.contains('.')
            || domain.starts_with('.')
            || domain.ends_with('.')
            || s.chars().any(char::is_whitespace)
        {
            return Err(ApiError::field("email", "Invalid email."));
        }
        return Ok((
            e.clone(),
            json!(format!("{}@{}", local, domain.to_lowercase())),
        ));
    }
    Ok((Value::Null, Value::Null))
}
pub(crate) fn array(v: &Value, k: &str, choices: Option<&[&str]>) -> Result<Value> {
    let Some(a) = v.get(k) else {
        return Ok(json!([]));
    };
    let a = a
        .as_array()
        .ok_or_else(|| ApiError::field(k, "Expected an array."))?;
    if a.len() > 50 {
        return Err(ApiError::field(k, "At most 50 values."));
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in a {
        let s = item
            .as_str()
            .map(str::trim)
            .ok_or_else(|| ApiError::field(k, "Expected text values."))?;
        if s.is_empty()
            || s.chars().count() > 200
            || !seen.insert(s.to_owned())
            || choices.is_some_and(|c| !c.contains(&s))
        {
            return Err(ApiError::field(
                k,
                "Values must be distinct, non-empty and supported.",
            ));
        }
        out.push(s.to_owned());
    }
    Ok(json!(out))
}
pub fn criteria(v: &Value) -> Result<Value> {
    object(
        v,
        &[
            "role",
            "keywords",
            "excluded_keywords",
            "locations",
            "technologies",
            "hh",
            "work_formats",
            "employment_types",
        ],
    )?;
    let mut criteria = json!({"role":optional(v,"role",500)?,
        "keywords":array(v,"keywords",None)?,"excluded_keywords":array(v,"excluded_keywords",None)?,
        "locations":array(v,"locations",None)?,"technologies":array(v,"technologies",None)?,
        "work_formats":array(v,"work_formats",Some(FORMATS))?,"employment_types":array(v,"employment_types",Some(EMPLOYMENT))?});
    if let Some(hh) = v.get("hh") {
        criteria["hh"] = crate::hh::query::normalize(hh)?;
    }
    Ok(criteria)
}
pub fn vacancy(v: &Value) -> Result<Value> {
    object(
        v,
        &[
            "company_id",
            "title",
            "description",
            "location",
            "work_format",
            "employment_type",
            "salary_min",
            "salary_max",
            "salary_currency",
            "public_url",
        ],
    )?;
    let mut salaries = [Value::Null, Value::Null];
    for (i, k) in ["salary_min", "salary_max"].iter().enumerate() {
        if v.get(*k).is_some_and(|x| !x.is_null()) {
            let n = v[*k]
                .as_i64()
                .filter(|n| (0..=9_007_199_254_740_991).contains(n))
                .ok_or_else(|| {
                    ApiError::field(k, "Use a non-negative safe integer in minor units.")
                })?;
            salaries[i] = json!(n);
        }
    }
    if salaries[0]
        .as_i64()
        .zip(salaries[1].as_i64())
        .is_some_and(|(min, max)| min > max)
    {
        return Err(ApiError::field(
            "salary_max",
            "Must be at least salary_min.",
        ));
    }
    let currency = optional(v, "salary_currency", 3)?;
    if currency
        .as_str()
        .is_some_and(|s| s.len() != 3 || !s.bytes().all(|c| c.is_ascii_uppercase()))
        || (salaries.iter().any(|s| !s.is_null()) && currency.is_null())
    {
        return Err(ApiError::field(
            "salary_currency",
            "Three uppercase currency letters required with a salary.",
        ));
    }
    Ok(
        json!({"company_id":uuid(v,"company_id")?,"title":required(v,"title",500)?,
       "description":optional(v,"description",100000)?,"location":optional(v,"location",500)?,
       "work_format":enum_optional(v,"work_format",FORMATS)?,"employment_type":enum_optional(v,"employment_type",EMPLOYMENT)?,
       "salary_min":salaries[0],"salary_max":salaries[1],"salary_currency":currency,"public_url":url(v,"public_url")?}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalization_preserves_source_path_query_and_fragment() {
        assert_eq!(
            normalize_url("HTTPS://Example.COM:443/A%2fb/?b=2&a=1#x").unwrap(),
            "https://example.com/A%2fb/?b=2&a=1#x"
        );
    }
    #[test]
    fn timestamp_offsets_normalize_without_precision_loss() {
        assert_eq!(
            time(&json!({"t":"2026-10-09T15:00:00+03:00"}), "t").unwrap(),
            "2026-10-09T12:00:00.000Z"
        );
    }
    #[test]
    fn timestamp_rejects_naive_and_submillisecond_input() {
        for t in ["2026-10-09T12:00:00", "2026-10-09T12:00:00.000001Z"] {
            assert!(time(&json!({"t":t}), "t").is_err());
        }
    }
}
