use crate::{
    error::{ApiError, Result},
    storage as s, validation as v,
};
use serde_json::{Value, json};
use sqlx::SqliteConnection;

pub fn normalize_vacancy_input(input: &Value) -> Result<Value> {
    v::object(
        input,
        &[
            "source_id",
            "external_vacancy_id",
            "external_url",
            "target_vacancy_id",
            "vacancy",
            "observation",
        ],
    )?;
    let external = v::optional(input, "external_vacancy_id", 500)?;
    let url = v::url(input, "external_url")?;
    if external.is_null() && url.is_null() {
        return Err(ApiError::field(
            "external_vacancy_id",
            "Provide an external ID or URL.",
        ));
    }
    let o = &input["observation"];
    v::object(
        o,
        &[
            "observation_key",
            "observed_at",
            "search_configuration_id",
            "source_payload",
        ],
    )?;
    let mut normalized = json!({"source_id":v::uuid(input,"source_id")?,"external_vacancy_id":external,"external_url":url,
        "target_vacancy_id":v::optional_uuid(input,"target_vacancy_id")?,"vacancy":v::vacancy(&input["vacancy"])?,
        "observation":{"observation_key":v::uuid(o,"observation_key")?,"observed_at":v::time(o,"observed_at")?,"search_configuration_id":v::optional_uuid(o,"search_configuration_id")?}});
    // Do not add defaults to old receipt shapes: existing exact retries must remain valid.
    if let Some(payload) = o.get("source_payload") {
        let (source_id, metadata) = match payload["provider"].as_str() {
            Some("hh") => (
                "20000000-0000-4000-8000-000000000003",
                crate::hh::source_payload(payload)?,
            ),
            Some("hh-browser") => (
                crate::browser_review::SOURCE_ID,
                crate::browser_review::source_payload(payload)?,
            ),
            _ => {
                return Err(ApiError::field(
                    "source_payload",
                    "Unsupported provenance provider.",
                ));
            }
        };
        if source_id == crate::browser_review::SOURCE_ID
            && (metadata["source_url"] != normalized["external_url"]
                || (metadata["classification"] == "relevant"
                    && normalized["vacancy"]["work_format"] != "remote"))
        {
            return Err(ApiError::field(
                "source_payload",
                "Browser review requires its canonical URL; relevant reviews require remote format.",
            ));
        }
        normalized["observation"]["source_payload"] = metadata;
        if normalized["source_id"] != source_id
            || payload["external_vacancy_id"] != normalized["external_vacancy_id"]
        {
            return Err(ApiError::field(
                "source_payload",
                "Metadata must match the declared source and vacancy ID.",
            ));
        }
    }
    if normalized["source_id"] == crate::browser_review::SOURCE_ID
        && o.get("source_payload").is_none()
    {
        return Err(ApiError::field(
            "source_payload",
            "Browser review provenance is required for this source.",
        ));
    }
    Ok(normalized)
}
async fn lookup(
    c: &mut SqliteConnection,
    t: &str,
    column: &str,
    parent_column: &str,
    parent: &str,
    value: Option<&str>,
) -> Result<Option<Value>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let data = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!(
        "SELECT data FROM {t} WHERE {parent_column}=? AND {column}=?"
    )))
    .bind(parent)
    .bind(value)
    .fetch_optional(c)
    .await?;
    data.map(|x| serde_json::from_str(&x).map_err(|_| ApiError::internal()))
        .transpose()
}
fn bounds(data: &mut Value, first: &str, last: &str, time: &str) {
    if data[first].as_str().is_none_or(|x| time < x) {
        data[first] = json!(time);
    }
    if data[last].as_str().is_none_or(|x| time > x) {
        data[last] = json!(time);
    }
}
pub async fn vacancy(c: &mut SqliteConnection, input: &Value) -> Result<(Value, bool)> {
    let source = input["source_id"].as_str().ok_or_else(ApiError::internal)?;
    s::exists(c, "vacancy_source", &input["source_id"], "source_id").await?;
    s::exists(c, "company", &input["vacancy"]["company_id"], "company_id").await?;
    s::exists(
        c,
        "search_configuration",
        &input["observation"]["search_configuration_id"],
        "search_configuration_id",
    )
    .await?;
    let normalized = input["external_url"]
        .as_str()
        .map(v::normalize_url)
        .transpose()?;
    let by_id = lookup(
        c,
        "vacancy_source_reference",
        "external_vacancy_id",
        "source_id",
        source,
        input["external_vacancy_id"].as_str(),
    )
    .await?;
    let by_url = lookup(
        c,
        "vacancy_source_reference",
        "normalized_url",
        "source_id",
        source,
        normalized.as_deref(),
    )
    .await?;
    if by_id
        .as_ref()
        .zip(by_url.as_ref())
        .is_some_and(|(a, b)| a["id"] != b["id"])
    {
        return Err(ApiError::conflict("identity_conflict"));
    }
    let existing = by_id.or(by_url);
    let mut vacancy_created = false;
    let reference_created = existing.is_none();
    let mut reference = if let Some(mut d) = existing {
        if input["target_vacancy_id"]
            .as_str()
            .is_some_and(|id| d["vacancy_id"] != id)
        {
            return Err(ApiError::conflict("identity_conflict"));
        }
        let canonical = s::get(
            c,
            "vacancy",
            d["vacancy_id"].as_str().ok_or_else(ApiError::internal)?,
        )
        .await?;
        if canonical["company_id"] != input["vacancy"]["company_id"] {
            return Err(ApiError::conflict("identity_conflict"));
        }
        if !d["external_vacancy_id"].is_null()
            && !input["external_vacancy_id"].is_null()
            && d["external_vacancy_id"] != input["external_vacancy_id"]
        {
            return Err(ApiError::conflict("identity_conflict"));
        }
        if !d["external_url"].is_null()
            && normalized.as_ref().is_some_and(|n| {
                d["external_url"]
                    .as_str()
                    .and_then(|u| v::normalize_url(u).ok())
                    .as_ref()
                    != Some(n)
            })
        {
            return Err(ApiError::conflict("identity_conflict"));
        }
        if d["external_vacancy_id"].is_null() {
            d["external_vacancy_id"] = input["external_vacancy_id"].clone();
        }
        if d["external_url"].is_null() {
            d["external_url"] = input["external_url"].clone();
        }
        d
    } else {
        let vacancy_id = if let Some(target) = input["target_vacancy_id"].as_str() {
            let d = s::get(c, "vacancy", target).await.map_err(|_| {
                ApiError::field("target_vacancy_id", "Target vacancy does not exist.")
            })?;
            if d["company_id"] != input["vacancy"]["company_id"] {
                return Err(ApiError::field(
                    "target_vacancy_id",
                    "Target must belong to the same company.",
                ));
            }
            target.to_owned()
        } else {
            let mut d = s::record(input["vacancy"].clone());
            d["status"] = json!("new");
            d["notes"] = Value::Null;
            d["first_seen_at"] = Value::Null;
            d["last_seen_at"] = Value::Null;
            s::insert(c, "vacancy", &d, &[("company_id", d["company_id"].clone())]).await?;
            vacancy_created = true;
            d["id"].as_str().ok_or_else(ApiError::internal)?.to_owned()
        };
        let d = s::record(
            json!({"vacancy_id":vacancy_id,"source_id":source,"external_vacancy_id":input["external_vacancy_id"],"external_url":input["external_url"],
            "link_basis":if input["target_vacancy_id"].is_null() {"new"}else{"explicit"},"normalization_version":"url-v1","first_seen_at":null,"last_seen_at":null}),
        );
        s::insert(
            c,
            "vacancy_source_reference",
            &d,
            &[
                ("vacancy_id", d["vacancy_id"].clone()),
                ("source_id", json!(source)),
                ("external_vacancy_id", d["external_vacancy_id"].clone()),
                (
                    "normalized_url",
                    normalized.clone().map(Value::String).unwrap_or(Value::Null),
                ),
            ],
        )
        .await?;
        d
    };
    let reference_id = reference["id"]
        .as_str()
        .ok_or_else(ApiError::internal)?
        .to_owned();
    let vacancy_id = reference["vacancy_id"]
        .as_str()
        .ok_or_else(ApiError::internal)?
        .to_owned();
    let observation = &input["observation"];
    let existing = lookup(
        c,
        "vacancy_discovery",
        "observation_key",
        "source_reference_id",
        &reference_id,
        observation["observation_key"].as_str(),
    )
    .await?;
    let (discovery_id, duplicate) = if let Some(d) = existing {
        if d["normalized_payload"] != input["vacancy"]
            || d["source_payload"] != observation["source_payload"]
            || d["observed_at"] != observation["observed_at"]
            || d["search_configuration_id"] != observation["search_configuration_id"]
        {
            return Err(ApiError::conflict("identity_conflict"));
        }
        (d["id"].clone(), true)
    } else {
        let snapshot = if let Some(config) = observation["search_configuration_id"].as_str() {
            let d = s::get(c, "search_configuration", config).await?;
            json!({"criteria_version":d["criteria_version"],"criteria":d["criteria"]})
        } else {
            Value::Null
        };
        let mut d = s::record(
            json!({"vacancy_id":vacancy_id,"source_reference_id":reference_id,"search_configuration_id":observation["search_configuration_id"],
            "observation_key":observation["observation_key"],"observed_at":observation["observed_at"],"discovered_at":v::now(),"normalized_payload":input["vacancy"],"criteria_snapshot":snapshot}),
        );
        if let Some(payload) = observation.get("source_payload") {
            d["source_payload"] = payload.clone();
        }
        d.as_object_mut()
            .ok_or_else(ApiError::internal)?
            .remove("updated_at");
        s::insert(
            c,
            "vacancy_discovery",
            &d,
            &[
                ("vacancy_id", json!(vacancy_id)),
                ("source_reference_id", json!(reference_id)),
                (
                    "search_configuration_id",
                    observation["search_configuration_id"].clone(),
                ),
                ("observation_key", observation["observation_key"].clone()),
            ],
        )
        .await?;
        (d["id"].clone(), false)
    };
    let time = observation["observed_at"]
        .as_str()
        .ok_or_else(ApiError::internal)?;
    bounds(&mut reference, "first_seen_at", "last_seen_at", time);
    reference["updated_at"] = json!(v::now());
    let normalized = reference["external_url"]
        .as_str()
        .map(v::normalize_url)
        .transpose()?
        .map(Value::String)
        .unwrap_or(Value::Null);
    s::update(
        c,
        "vacancy_source_reference",
        &reference,
        &[
            (
                "external_vacancy_id",
                reference["external_vacancy_id"].clone(),
            ),
            ("normalized_url", normalized),
        ],
    )
    .await?;
    let mut canonical = s::get(c, "vacancy", &vacancy_id).await?;
    bounds(&mut canonical, "first_seen_at", "last_seen_at", time);
    canonical["updated_at"] = json!(v::now());
    s::update(c, "vacancy", &canonical, &[]).await?;
    Ok((
        json!({"vacancy_id":vacancy_id,"source_reference_id":reference_id,"discovery_id":discovery_id,
        "vacancy_created":vacancy_created,"reference_created":reference_created,"discovery_created":!duplicate}),
        duplicate,
    ))
}
pub fn normalize_message(input: &Value) -> Result<Value> {
    v::object(
        input,
        &[
            "direction",
            "sender_participant_id",
            "body",
            "occurred_at",
            "external_message_id",
            "client_message_id",
        ],
    )?;
    let direction = v::required(input, "direction", 30)?;
    if !["inbound", "outbound"].contains(&direction.as_str()) {
        return Err(ApiError::field("direction", "Use inbound or outbound."));
    }
    let body = input["body"]
        .as_str()
        .filter(|b| !b.trim().is_empty() && b.chars().count() <= 100000)
        .ok_or_else(|| {
            ApiError::field("body", "Required message text, up to 100000 characters.")
        })?;
    let external = v::optional(input, "external_message_id", 500)?;
    let client = v::optional_uuid(input, "client_message_id")?;
    if external.is_null() && client.is_null() {
        return Err(ApiError::field(
            "client_message_id",
            "A stable client UUID or external ID is required.",
        ));
    }
    Ok(
        json!({"direction":direction,"sender_participant_id":v::uuid(input,"sender_participant_id")?,"body":body,
        "occurred_at":v::time(input,"occurred_at")?,"external_message_id":external,"client_message_id":client}),
    )
}
pub async fn message(
    c: &mut SqliteConnection,
    conversation_id: &str,
    input: &Value,
) -> Result<(Value, bool)> {
    let mut conversation = s::get(c, "conversation", conversation_id).await?;
    let sender = s::get(
        c,
        "conversation_participant",
        input["sender_participant_id"]
            .as_str()
            .ok_or_else(ApiError::internal)?,
    )
    .await
    .map_err(|_| ApiError::field("sender_participant_id", "Sender does not exist."))?;
    if sender["conversation_id"] != conversation_id
        || ((input["direction"] == "outbound") != (sender["participant_kind"] == "user"))
    {
        return Err(ApiError::field(
            "sender_participant_id",
            "Sender must belong to this conversation and match the direction.",
        ));
    }
    let external = lookup(
        c,
        "message",
        "external_message_id",
        "conversation_id",
        conversation_id,
        input["external_message_id"].as_str(),
    )
    .await?;
    let client = lookup(
        c,
        "message",
        "client_message_id",
        "conversation_id",
        conversation_id,
        input["client_message_id"].as_str(),
    )
    .await?;
    if external
        .as_ref()
        .zip(client.as_ref())
        .is_some_and(|(a, b)| a["id"] != b["id"])
    {
        return Err(ApiError::conflict("message_conflict"));
    }
    if let Some(existing) = external.or(client) {
        if ["direction", "sender_participant_id", "body", "occurred_at"]
            .iter()
            .any(|k| existing[*k] != input[*k])
            || ["external_message_id", "client_message_id"]
                .iter()
                .any(|k| !input[*k].is_null() && existing[*k] != input[*k])
        {
            return Err(ApiError::conflict("message_conflict"));
        }
        return Ok((existing, true));
    }
    let mut d = s::record(input.clone());
    d["conversation_id"] = json!(conversation_id);
    d["sender_contact_id"] = sender["contact_id"].clone();
    d["received_at"] = json!(v::now());
    s::insert(
        c,
        "message",
        &d,
        &[
            ("conversation_id", json!(conversation_id)),
            ("sender_participant_id", d["sender_participant_id"].clone()),
            ("external_message_id", d["external_message_id"].clone()),
            ("client_message_id", d["client_message_id"].clone()),
        ],
    )
    .await?;
    bounds(
        &mut conversation,
        "started_at",
        "last_message_at",
        input["occurred_at"]
            .as_str()
            .ok_or_else(ApiError::internal)?,
    );
    conversation["updated_at"] = json!(v::now());
    s::update(c, "conversation", &conversation, &[]).await?;
    Ok((d, false))
}
