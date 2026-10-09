use super::query::MAX_RESULTS;
use crate::{
    error::{ApiError, Result},
    validation as v,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn bad() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "hh_invalid_response",
        "HH returned an incompatible response. No records were imported.",
    )
}
fn bounded(value: &Value, max: usize) -> Option<String> {
    value.as_str().map(|x| x.chars().take(max).collect())
}
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars().take(100_000) {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}
fn candidate(item: &Value, hh: &Value, page: u64, fetched: &str) -> Result<Value> {
    let id = item["id"]
        .as_str()
        .filter(|x| !x.is_empty() && x.len() <= 20 && x.bytes().all(|c| c.is_ascii_digit()))
        .ok_or_else(bad)?;
    let title = item["name"]
        .as_str()
        .filter(|x| !x.trim().is_empty() && x.chars().count() <= 500)
        .ok_or_else(bad)?;
    let url = format!("https://hh.ru/vacancy/{id}");
    let company = bounded(&item["employer"]["name"], 300)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Employer not disclosed by HH".into());
    let formats = item["work_format"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x["id"].as_str())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let format = if formats.len() == 1 {
        match formats[0] {
            "REMOTE" => Some("remote"),
            "ON_SITE" => Some("onsite"),
            "HYBRID" => Some("hybrid"),
            _ => None,
        }
    } else {
        None
    };
    let form = item["employment_form"]["id"].as_str();
    let employment = match form {
        Some("FULL") => Some("full_time"),
        Some("PART") => Some("part_time"),
        Some("PROJECT") => Some("contract"),
        _ => None,
    };
    let description = plain(
        &[
            item["snippet"]["requirement"].as_str().unwrap_or(""),
            item["snippet"]["responsibility"].as_str().unwrap_or(""),
        ]
        .join("\n"),
    );
    let salary = item
        .get("salary_range")
        .filter(|x| x.is_object())
        .or_else(|| item.get("salary").filter(|x| x.is_object()));
    let salary_metadata=salary.map(|s|json!({"from":s["from"],"to":s["to"],"currency":s["currency"],"gross":s["gross"],"mode":s["mode"],"frequency":s["frequency"]})).unwrap_or(Value::Null);
    let currency = salary.and_then(|s| s["currency"].as_str());
    let iso = match currency {
        Some("RUR") => Some("RUB"),
        Some(
            c @ ("USD" | "EUR" | "RUB" | "BYN" | "KZT" | "UAH" | "AZN" | "UZS" | "GBP" | "CHF"),
        ) => Some(c),
        _ => None,
    };
    let minor = |key: &str| {
        salary
            .and_then(|s| s[key].as_u64())
            .and_then(|n| n.checked_mul(100))
            .filter(|n| *n <= 9_007_199_254_740_991)
    };
    let min = iso.and_then(|_| minor("from"));
    let max = iso.and_then(|_| minor("to"));
    let (min, max) = if min.zip(max).is_some_and(|(a, b)| a > b) {
        (None, None)
    } else {
        (min, max)
    };
    let published = item["published_at"]
        .as_str()
        .and_then(|x| {
            chrono::DateTime::parse_from_rfc3339(x)
                .or_else(|_| chrono::DateTime::parse_from_str(x, "%Y-%m-%dT%H:%M:%S%z"))
                .ok()
        })
        .map(|x| {
            x.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        });
    Ok(
        json!({"external_vacancy_id":id,"external_url":url,"company_name":company,
        "vacancy":{"title":title,"description":description,"location":bounded(&item["area"]["name"],500),"work_format":format,
            "employment_type":employment,"salary_min":min,"salary_max":max,"salary_currency":if min.is_some()||max.is_some(){iso}else{None},"public_url":url},
        "source_payload":{"provider":"hh","api_url":"https://api.hh.ru/vacancies","external_vacancy_id":id,
            "employer_id":bounded(&item["employer"]["id"],100),"employer_name":company,"published_at":published,
            "description_kind":"search_snippet","salary":salary_metadata,"work_formats":formats,"employment_form":form,
            "query":hh,"page":page,"fetched_at":fetched}}),
    )
}
pub fn preview(
    payload: Value,
    config: &Value,
    hh: &Value,
    page: u64,
    fetched: &str,
) -> Result<Value> {
    let items = payload["items"].as_array().ok_or_else(bad)?;
    let returned_page = payload["page"]
        .as_u64()
        .filter(|x| *x == page)
        .ok_or_else(bad)?;
    let size = hh["per_page"].as_u64().ok_or_else(bad)?;
    if payload["per_page"].as_u64() != Some(size) || items.len() as u64 > size {
        return Err(bad());
    }
    let pages = payload["pages"].as_u64().ok_or_else(bad)?;
    let found = payload["found"].as_u64().ok_or_else(bad)?;
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut skipped = 0;
    for item in items {
        match candidate(item, hh, page, fetched) {
            Ok(row)
                if seen.insert(
                    row["external_vacancy_id"]
                        .as_str()
                        .ok_or_else(bad)?
                        .to_owned(),
                ) =>
            {
                candidates.push(row)
            }
            _ => skipped += 1,
        }
    }
    let accessible_pages = pages.min(MAX_RESULTS / size);
    let next = if page + 1 < accessible_pages {
        Some(page + 1)
    } else {
        None
    };
    Ok(
        json!({"adapter":"hh","live":true,"configuration_id":config["id"],"scanned":items.len(),"matched":candidates.len(),
        "candidates":candidates,"page":returned_page,"per_page":size,"pages":pages,"found":found,"accessible_pages":accessible_pages,
        "next_page":next,"skipped":skipped,"fetched_at":fetched,"result_limit":MAX_RESULTS,
        "notice":"HH search semantics apply. Descriptions are snippets. Results may shift between pages; nothing is imported automatically."}),
    )
}
pub fn source_payload(value: &Value) -> Result<Value> {
    v::object(
        value,
        &[
            "provider",
            "api_url",
            "external_vacancy_id",
            "employer_id",
            "employer_name",
            "published_at",
            "description_kind",
            "salary",
            "work_formats",
            "employment_form",
            "query",
            "page",
            "fetched_at",
        ],
    )?;
    if value["provider"] != "hh"
        || value["api_url"] != "https://api.hh.ru/vacancies"
        || value["description_kind"] != "search_snippet"
        || value["page"].as_u64().is_none_or(|x| x >= 200)
    {
        return Err(ApiError::field(
            "source_payload",
            "Invalid HH provenance metadata.",
        ));
    }
    if value.to_string().len() > 32_768 {
        return Err(ApiError::field(
            "source_payload",
            "Metadata exceeds 32 KiB.",
        ));
    }
    v::required(value, "external_vacancy_id", 20)?;
    v::required(value, "employer_name", 300)?;
    v::optional(value, "employer_id", 100)?;
    v::optional(value, "employment_form", 100)?;
    if !value["published_at"].is_null() {
        v::time(value, "published_at")?;
    }
    v::time(value, "fetched_at")?;
    super::query::normalize(&value["query"])?;
    if !value["salary"].is_null() {
        v::object(
            &value["salary"],
            &["from", "to", "currency", "gross", "mode", "frequency"],
        )?;
    }
    if !value["work_formats"].as_array().is_some_and(|a| {
        a.len() <= 10 && a.iter().all(|x| x.as_str().is_some_and(|s| s.len() <= 100))
    }) {
        return Err(ApiError::field(
            "source_payload",
            "Invalid HH work formats.",
        ));
    }
    Ok(value.clone())
}
