//! Offline vacancy acquisition boundary. No adapter performs network requests.
use crate::error::{ApiError, Result};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Clone, Deserialize)]
pub struct Candidate {
    pub external_vacancy_id: String,
    pub external_url: String,
    pub company_name: String,
    pub title: String,
    pub description: String,
    pub location: String,
    pub work_format: String,
    pub employment_type: String,
}
pub trait VacancyAdapter {
    fn candidates(&self) -> Result<Vec<Candidate>>;
}
pub struct FixtureAdapter;
impl VacancyAdapter for FixtureAdapter {
    fn candidates(&self) -> Result<Vec<Candidate>> {
        serde_json::from_str(include_str!("../fixtures/vacancies.json"))
            .map_err(|_| ApiError::internal())
    }
}
fn contains(text: &str, needle: &str) -> bool {
    text.to_lowercase().contains(&needle.to_lowercase())
}
fn list<'a>(criteria: &'a Value, key: &str) -> Vec<&'a str> {
    criteria[key]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}
pub fn matches(candidate: &Candidate, criteria: &Value) -> bool {
    let searchable = format!("{} {}", candidate.title, candidate.description);
    criteria["role"]
        .as_str()
        .is_none_or(|role| contains(&candidate.title, role))
        && list(criteria, "keywords")
            .iter()
            .all(|word| contains(&searchable, word))
        && list(criteria, "excluded_keywords")
            .iter()
            .all(|word| !contains(&searchable, word))
        && list(criteria, "technologies")
            .iter()
            .all(|word| contains(&searchable, word))
        && (list(criteria, "locations").is_empty()
            || list(criteria, "locations")
                .iter()
                .any(|word| contains(&candidate.location, word)))
        && (list(criteria, "work_formats").is_empty()
            || list(criteria, "work_formats").contains(&candidate.work_format.as_str()))
        && (list(criteria, "employment_types").is_empty()
            || list(criteria, "employment_types").contains(&candidate.employment_type.as_str()))
}
pub fn preview(configuration: &Value) -> Result<Value> {
    let candidates = FixtureAdapter.candidates()?;
    let scanned = candidates.len();
    let matched:Vec<Value>=candidates.iter().filter(|candidate|matches(candidate,&configuration["criteria"])).map(|c|json!({
 "external_vacancy_id":c.external_vacancy_id,"external_url":c.external_url,"company_name":c.company_name,
 "vacancy":{"title":c.title,"description":c.description,"location":c.location,"work_format":c.work_format,"employment_type":c.employment_type,"public_url":c.external_url}
 })).collect();
    Ok(
        json!({"adapter":"fixture-v1","live":false,"configuration_id":configuration["id"],"scanned":scanned,"matched":matched.len(),"candidates":matched}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn criteria_match_keywords_exclusions_and_formats() {
        let rows = FixtureAdapter.candidates().unwrap();
        let criteria =
            json!({"keywords":["Rust"],"excluded_keywords":["unpaid"],"work_formats":["remote"]});
        assert_eq!(
            rows.iter()
                .filter(|r| matches(r, &criteria))
                .map(|r| r.external_vacancy_id.as_str())
                .collect::<Vec<_>>(),
            vec!["fixture-rust-remote"]
        );
    }
    #[test]
    fn locations_and_employment_use_any_within_dimension() {
        let rows = FixtureAdapter.candidates().unwrap();
        assert!(matches(
            &rows[2],
            &json!({"locations":["Berlin","London"],"employment_types":["contract"]})
        ));
    }
}
