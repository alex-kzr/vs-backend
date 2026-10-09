//! Allowlisted public facts from a user-directed browser review, never an API response.
use crate::{
    error::{ApiError, Result},
    validation as v,
};
use serde_json::{Value, json};

pub const SOURCE_ID: &str = "20000000-0000-4000-8000-000000000004";

pub fn source_payload(input: &Value) -> Result<Value> {
    if input.to_string().len() > 32 * 1024 {
        return Err(ApiError::field(
            "source_payload",
            "Browser review exceeds 32 KiB.",
        ));
    }
    v::object(
        input,
        &[
            "provider",
            "external_vacancy_id",
            "source_url",
            "employer_name",
            "published_date",
            "queries",
            "reviewed_at",
            "relevance_reason",
            "remote_evidence",
            "eligibility_notes",
            "salary_original",
            "classification",
        ],
    )?;
    if input["provider"] != "hh-browser" {
        return Err(ApiError::field("provider", "Expected hh-browser."));
    }
    let id = v::required(input, "external_vacancy_id", 30)?;
    if !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ApiError::field(
            "external_vacancy_id",
            "Expected a numeric HH vacancy ID.",
        ));
    }
    let source_url = v::required(input, "source_url", 2048)?;
    if source_url != format!("https://hh.ru/vacancy/{id}") {
        return Err(ApiError::field(
            "source_url",
            "Use the canonical HH vacancy URL matching its ID.",
        ));
    }
    let queries = v::array(input, "queries", None)?;
    if queries.as_array().is_none_or(Vec::is_empty) {
        return Err(ApiError::field(
            "queries",
            "Record at least one query used for this review.",
        ));
    }
    let date = v::optional(input, "published_date", 10)?;
    if let Some(date) = date.as_str() {
        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
            .map_err(|_| ApiError::field("published_date", "Use YYYY-MM-DD."))?;
    }
    let classification = v::required(input, "classification", 30)?;
    if !["relevant", "needs_review"].contains(&classification.as_str()) {
        return Err(ApiError::field(
            "classification",
            "Expected relevant or needs_review.",
        ));
    }
    Ok(
        json!({"provider":"hh-browser","external_vacancy_id":id,"source_url":source_url,
        "employer_name":v::required(input,"employer_name",300)?,"published_date":date,
        "queries":queries,"reviewed_at":v::time(input,"reviewed_at")?,
        "relevance_reason":v::required(input,"relevance_reason",2000)?,
        "remote_evidence":v::required(input,"remote_evidence",1000)?,
        "eligibility_notes":v::required(input,"eligibility_notes",2000)?,
        "salary_original":v::optional(input,"salary_original",500)?,"classification":classification}),
    )
}
