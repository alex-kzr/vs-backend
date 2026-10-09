use crate::{
    error::{ApiError, Result},
    ingestion, storage as s, validation as v,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};

fn normalize_company(input: &Value) -> Result<Value> {
    v::object(input, &["name", "website", "notes"])?;
    Ok(
        json!({"name":v::required(input,"name",300)?,"website":v::url(input,"website")?,"notes":v::optional(input,"notes",10000)?}),
    )
}
fn normalize_config(input: &Value) -> Result<Value> {
    v::object(input, &["name", "criteria_version", "criteria", "enabled"])?;
    if input["criteria_version"] != json!(1) {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "unsupported_criteria_version",
            "Only criteria version 1 is supported.",
        ));
    }
    Ok(
        json!({"name":v::required(input,"name",200)?,"criteria_version":1,"criteria":v::criteria(&input["criteria"])?,"enabled":v::boolean(input,"enabled",true)?}),
    )
}
fn normalize_contact(input: &Value) -> Result<Value> {
    v::object(
        input,
        &[
            "company_id",
            "display_name",
            "email",
            "profile_url",
            "notes",
            "role_ids",
        ],
    )?;
    let (email, _) = v::email(input)?;
    let roles = input.get("role_ids").cloned().unwrap_or(json!([]));
    let array = roles
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or_else(|| ApiError::field("role_ids", "Expected up to 100 distinct role IDs."))?;
    let mut ids = Vec::new();
    for role in array {
        let id = v::uuid(&json!({"role_ids":role}), "role_ids")?;
        if ids.contains(&id) {
            return Err(ApiError::field("role_ids", "Duplicate role."));
        }
        ids.push(id);
    }
    ids.sort();
    Ok(
        json!({"company_id":v::uuid(input,"company_id")?,"display_name":v::required(input,"display_name",300)?,
        "email":email,"profile_url":v::url(input,"profile_url")?,"notes":v::optional(input,"notes",10000)?,"role_ids":ids}),
    )
}
pub fn normalize_participant(input: &Value) -> Result<Value> {
    v::object(
        input,
        &[
            "participant_key",
            "participant_kind",
            "contact_id",
            "display_name",
        ],
    )?;
    let kind = v::required(input, "participant_kind", 30)?;
    if !["user", "contact", "unknown"].contains(&kind.as_str()) {
        return Err(ApiError::field(
            "participant_kind",
            "Unsupported participant kind.",
        ));
    }
    let contact = v::optional_uuid(input, "contact_id")?;
    if (kind == "contact") == contact.is_null() {
        return Err(ApiError::field(
            "contact_id",
            "Contact ID required only for contact participants.",
        ));
    }
    let label = v::optional(input, "display_name", 300)?;
    if kind == "unknown" && label.is_null() {
        return Err(ApiError::field(
            "display_name",
            "Label required for unknown participant.",
        ));
    }
    Ok(
        json!({"participant_key":v::required(input,"participant_key",200)?,"participant_kind":kind,"contact_id":contact,"display_name":label}),
    )
}
fn normalize_conversation(input: &Value) -> Result<Value> {
    v::object(
        input,
        &[
            "company_id",
            "vacancy_id",
            "channel_id",
            "external_conversation_id",
            "subject",
            "participants",
        ],
    )?;
    let items = input["participants"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 100)
        .ok_or_else(|| ApiError::field("participants", "Select 1-100 participants."))?;
    let mut participants = Vec::new();
    let mut keys = Vec::new();
    for item in items {
        let p = normalize_participant(item)?;
        if keys.contains(&p["participant_key"]) {
            return Err(ApiError::field(
                "participants",
                "Duplicate participant key.",
            ));
        }
        keys.push(p["participant_key"].clone());
        participants.push(p);
    }
    Ok(
        json!({"company_id":v::uuid(input,"company_id")?,"vacancy_id":v::optional_uuid(input,"vacancy_id")?,"channel_id":v::uuid(input,"channel_id")?,
        "external_conversation_id":v::optional(input,"external_conversation_id",500)?,"subject":v::optional(input,"subject",500)?,"participants":participants}),
    )
}
fn normalize(parts: &[&str], input: &Value) -> Result<Value> {
    match parts {
        ["companies"] => normalize_company(input),
        ["search-configurations"] => normalize_config(input),
        ["contacts"] => normalize_contact(input),
        ["conversations"] => normalize_conversation(input),
        ["conversations", _, "participants"] => normalize_participant(input),
        ["conversations", _, "messages"] => ingestion::normalize_message(input),
        ["vacancy-source-references"] => ingestion::normalize_vacancy_input(input),
        _ => Err(ApiError::missing()),
    }
}
async fn replay(c: &mut SqliteConnection, parts: &[&str], result: Value) -> Result<Value> {
    if parts == ["vacancy-source-references"] {
        let mut result = result;
        for flag in ["vacancy_created", "reference_created", "discovery_created"] {
            result[flag] = json!(false);
        }
        return Ok(result);
    }
    let t = match parts {
        ["conversations", _, "messages"] => "message",
        ["conversations", _, "participants"] => "conversation_participant",
        _ => s::table(parts[0])?,
    };
    let data = s::get(c, t, result["id"].as_str().ok_or_else(ApiError::internal)?).await?;
    s::hydrate(c, t, data, true).await
}
pub async fn create(
    c: &mut SqliteConnection,
    parts: &[&str],
    key: &str,
    input: Value,
) -> Result<(StatusCode, Value)> {
    let input = normalize(parts, &input)?;
    let operation = parts[0];
    let target = parts.get(1).copied().unwrap_or("");
    let operation = if parts.len() > 2 { parts[2] } else { operation };
    let receipt=sqlx::query("SELECT request_json,result_json FROM request_receipt WHERE operation=? AND target=? AND key=?")
        .bind(operation).bind(target).bind(key).fetch_optional(&mut *c).await?;
    if let Some(row) = receipt {
        let original: Value =
            serde_json::from_str(row.get("request_json")).map_err(|_| ApiError::internal())?;
        if original != input {
            return Err(ApiError::conflict("idempotency_conflict"));
        }
        let result =
            serde_json::from_str(row.get("result_json")).map_err(|_| ApiError::internal())?;
        return Ok((
            StatusCode::OK,
            json!({"data":replay(c,parts,result).await?,"deduplicated":true}),
        ));
    }
    let (data, duplicate) = match parts {
        ["companies"] => {
            let d = s::record(input.clone());
            s::insert(c, "company", &d, &[]).await?;
            (d, false)
        }
        ["search-configurations"] => {
            let d = s::record(input.clone());
            s::insert(
                c,
                "search_configuration",
                &d,
                &[("name", d["name"].clone())],
            )
            .await?;
            (d, false)
        }
        ["contacts"] => {
            let d = s::record(input.clone());
            save_contact(c, &d, true).await?;
            (s::hydrate(c, "contact", d, true).await?, false)
        }
        ["conversations"] => create_conversation(c, &input).await?,
        ["conversations", id, "participants"] => add_participant(c, id, &input).await?,
        ["conversations", id, "messages"] => ingestion::message(c, id, &input).await?,
        ["vacancy-source-references"] => ingestion::vacancy(c, &input).await?,
        _ => return Err(ApiError::missing()),
    };
    let result = if parts == ["vacancy-source-references"] {
        data.clone()
    } else {
        json!({"id":data["id"]})
    };
    sqlx::query("INSERT INTO request_receipt(operation,target,key,request_json,result_json) VALUES(?,?,?,?,?)")
        .bind(operation).bind(target).bind(key).bind(input.to_string()).bind(result.to_string()).execute(c).await?;
    Ok((
        if duplicate {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        },
        json!({"data":data,"deduplicated":duplicate}),
    ))
}
async fn save_contact(c: &mut SqliteConnection, data: &Value, new: bool) -> Result<()> {
    s::exists(c, "company", &data["company_id"], "company_id").await?;
    for id in data["role_ids"].as_array().ok_or_else(ApiError::internal)? {
        s::exists(c, "contact_role", id, "role_ids").await?;
    }
    let (_, email) = v::email(data)?;
    let profile = data["profile_url"]
        .as_str()
        .map(v::normalize_url)
        .transpose()?
        .map(Value::String)
        .unwrap_or(Value::Null);
    let columns = [
        ("company_id", data["company_id"].clone()),
        ("normalized_email", email),
        ("normalized_profile_url", profile),
    ];
    if new {
        s::insert(c, "contact", data, &columns).await?;
    } else {
        s::update(c, "contact", data, &columns).await?;
    }
    sqlx::query("DELETE FROM contact_role_assignment WHERE contact_id=?")
        .bind(data["id"].as_str())
        .execute(&mut *c)
        .await?;
    for id in data["role_ids"].as_array().ok_or_else(ApiError::internal)? {
        sqlx::query("INSERT INTO contact_role_assignment(contact_id,role_id) VALUES(?,?)")
            .bind(data["id"].as_str())
            .bind(id.as_str())
            .execute(&mut *c)
            .await?;
    }
    Ok(())
}
pub async fn add_participant(
    c: &mut SqliteConnection,
    conversation_id: &str,
    input: &Value,
) -> Result<(Value, bool)> {
    let conversation = s::get(c, "conversation", conversation_id).await?;
    let existing = sqlx::query_scalar::<_, String>(
        "SELECT data FROM conversation_participant WHERE conversation_id=? AND participant_key=?",
    )
    .bind(conversation_id)
    .bind(input["participant_key"].as_str())
    .fetch_optional(&mut *c)
    .await?;
    if let Some(existing) = existing {
        let d: Value = serde_json::from_str(&existing).map_err(|_| ApiError::internal())?;
        if ["participant_key", "participant_kind", "contact_id"]
            .iter()
            .any(|k| d[*k] != input[*k])
            || (!input["display_name"].is_null() && d["display_name"] != input["display_name"])
        {
            return Err(ApiError::conflict("identity_conflict"));
        }
        return Ok((d, true));
    }
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM conversation_participant WHERE conversation_id=?")
            .bind(conversation_id)
            .fetch_one(&mut *c)
            .await?;
    if count >= 100 {
        return Err(ApiError::field("participants", "At most 100 participants."));
    }
    let mut d = s::record(input.clone());
    d["conversation_id"] = json!(conversation_id);
    if let Some(contact_id) = input["contact_id"].as_str() {
        let contact = s::get(c, "contact", contact_id)
            .await
            .map_err(|_| ApiError::field("contact_id", "Contact does not exist."))?;
        if contact["company_id"] != conversation["company_id"] {
            return Err(ApiError::field(
                "contact_id",
                "Contact must belong to the conversation company.",
            ));
        }
        if d["display_name"].is_null() {
            d["display_name"] = contact["display_name"].clone();
        }
    }
    if d["display_name"].is_null() {
        d["display_name"] = json!("Me");
    }
    s::insert(
        c,
        "conversation_participant",
        &d,
        &[
            ("conversation_id", json!(conversation_id)),
            ("participant_key", d["participant_key"].clone()),
            ("participant_kind", d["participant_kind"].clone()),
            ("contact_id", d["contact_id"].clone()),
        ],
    )
    .await?;
    Ok((d, false))
}
async fn create_conversation(c: &mut SqliteConnection, input: &Value) -> Result<(Value, bool)> {
    s::exists(c, "company", &input["company_id"], "company_id").await?;
    s::exists(
        c,
        "communication_channel",
        &input["channel_id"],
        "channel_id",
    )
    .await?;
    if let Some(id) = input["vacancy_id"].as_str() {
        let vacancy = s::get(c, "vacancy", id)
            .await
            .map_err(|_| ApiError::field("vacancy_id", "Vacancy does not exist."))?;
        if vacancy["company_id"] != input["company_id"] {
            return Err(ApiError::field(
                "vacancy_id",
                "Vacancy must belong to the company.",
            ));
        }
    }
    if let Some(external) = input["external_conversation_id"].as_str() {
        let existing = sqlx::query_scalar::<_, String>(
            "SELECT data FROM conversation WHERE channel_id=? AND external_conversation_id=?",
        )
        .bind(input["channel_id"].as_str())
        .bind(external)
        .fetch_optional(&mut *c)
        .await?;
        if let Some(existing) = existing {
            let d: Value = serde_json::from_str(&existing).map_err(|_| ApiError::internal())?;
            if ["company_id", "vacancy_id", "subject"]
                .iter()
                .any(|k| d[*k] != input[*k])
            {
                return Err(ApiError::conflict("identity_conflict"));
            }
            let id = d["id"].as_str().ok_or_else(ApiError::internal)?;
            for p in input["participants"]
                .as_array()
                .ok_or_else(ApiError::internal)?
            {
                add_participant(c, id, p).await?;
            }
            return Ok((s::hydrate(c, "conversation", d, true).await?, true));
        }
    }
    let mut d = s::record(input.clone());
    d.as_object_mut()
        .ok_or_else(ApiError::internal)?
        .remove("participants");
    d["started_at"] = Value::Null;
    d["last_message_at"] = Value::Null;
    s::insert(
        c,
        "conversation",
        &d,
        &[
            ("company_id", d["company_id"].clone()),
            ("vacancy_id", d["vacancy_id"].clone()),
            ("channel_id", d["channel_id"].clone()),
            (
                "external_conversation_id",
                d["external_conversation_id"].clone(),
            ),
        ],
    )
    .await?;
    let id = d["id"].as_str().ok_or_else(ApiError::internal)?;
    for p in input["participants"]
        .as_array()
        .ok_or_else(ApiError::internal)?
    {
        add_participant(c, id, p).await?;
    }
    Ok((s::hydrate(c, "conversation", d, true).await?, false))
}
pub async fn patch(c: &mut SqliteConnection, parts: &[&str], input: Value) -> Result<Value> {
    if parts.len() != 2 {
        return Err(ApiError::missing());
    }
    let t = s::table(parts[0])?;
    let allowed: &[&str] = match t {
        "company" => &["name", "website", "notes"],
        "search_configuration" => &["name", "criteria_version", "criteria", "enabled"],
        "contact" => &["display_name", "email", "profile_url", "notes", "role_ids"],
        "vacancy" => &["status", "notes"],
        _ => return Err(ApiError::missing()),
    };
    v::object(&input, allowed)?;
    if input.as_object().is_none_or(|m| m.is_empty()) {
        return Err(ApiError::field("body", "Supply at least one field."));
    }
    let mut data = s::get(c, t, parts[1]).await?;
    for (k, value) in input.as_object().ok_or_else(ApiError::internal)? {
        data[k] = value.clone();
    }
    let mut fields = json!({});
    for k in allowed {
        fields[*k] = data[*k].clone();
    }
    let normalized = match t {
        "company" => normalize_company(&fields)?,
        "search_configuration" => normalize_config(&fields)?,
        "contact" => {
            fields["company_id"] = data["company_id"].clone();
            normalize_contact(&fields)?
        }
        "vacancy" => {
            let status = v::required(&fields, "status", 30)?;
            if !v::STATUSES.contains(&status.as_str()) {
                return Err(ApiError::field("status", "Unsupported status."));
            }
            json!({"status":status,"notes":v::optional(&fields,"notes",10000)?})
        }
        _ => return Err(ApiError::missing()),
    };
    for (k, value) in normalized.as_object().ok_or_else(ApiError::internal)? {
        data[k] = value.clone();
    }
    data["updated_at"] = json!(v::now());
    match t {
        "contact" => save_contact(c, &data, false).await?,
        "search_configuration" => s::update(c, t, &data, &[("name", data["name"].clone())]).await?,
        _ => s::update(c, t, &data, &[]).await?,
    }
    s::hydrate(c, t, data, true).await
}
