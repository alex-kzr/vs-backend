use crate::{
    error::{ApiError, Result},
    validation as v,
};
use axum::http::StatusCode;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
use std::collections::BTreeMap;

pub fn table(resource: &str) -> Result<&'static str> {
    match resource {
        "companies" => Ok("company"),
        "search-configurations" => Ok("search_configuration"),
        "vacancies" => Ok("vacancy"),
        "vacancy-sources" => Ok("vacancy_source"),
        "contacts" => Ok("contact"),
        "contact-roles" => Ok("contact_role"),
        "conversations" => Ok("conversation"),
        "communication-channels" => Ok("communication_channel"),
        _ => Err(ApiError::missing()),
    }
}
pub async fn find(c: &mut SqliteConnection, t: &str, id: &str) -> Result<Option<Value>> {
    let row = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(format!(
        "SELECT data FROM {t} WHERE id=?"
    )))
    .bind(id)
    .fetch_optional(c)
    .await?;
    row.map(|s| serde_json::from_str(&s).map_err(|_| ApiError::internal()))
        .transpose()
}
pub async fn get(c: &mut SqliteConnection, t: &str, id: &str) -> Result<Value> {
    find(c, t, id).await?.ok_or_else(ApiError::missing)
}
pub async fn exists(c: &mut SqliteConnection, t: &str, id: &Value, field: &str) -> Result<()> {
    if id.is_null() {
        return Ok(());
    }
    if find(
        c,
        t,
        id.as_str()
            .ok_or_else(|| ApiError::field(field, "Expected an ID."))?,
    )
    .await?
    .is_none()
    {
        return Err(ApiError::field(field, "Referenced record does not exist."));
    }
    Ok(())
}
pub fn record(mut data: Value) -> Value {
    data["id"] = json!(v::id());
    let now = v::now();
    data["created_at"] = json!(now);
    data["updated_at"] = json!(now);
    data
}
// All dynamic identifiers below originate in fixed resource/column allowlists; user values are bound.
fn normalized_key(table: &str) -> Option<&'static str> {
    match table {
        "company" => Some("name"),
        "contact" => Some("display_name"),
        "vacancy" => Some("title"),
        _ => None,
    }
}

pub async fn insert(
    c: &mut SqliteConnection,
    t: &str,
    data: &Value,
    columns: &[(&str, Value)],
) -> Result<()> {
    let mut columns = columns.to_vec();
    if let Some(key) = normalized_key(t) {
        columns.push((
            "normalized_text",
            json!(data[key].as_str().unwrap_or("").to_lowercase()),
        ));
    }
    let names = columns
        .iter()
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(",");
    let extras = if names.is_empty() {
        String::new()
    } else {
        format!(",{names}")
    };
    let sql = format!(
        "INSERT INTO {t}(id,data{extras}) VALUES (?,?{})",
        ",?".repeat(columns.len())
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(data["id"].as_str())
        .bind(data.to_string());
    for (_, value) in &columns {
        query = query.bind(value.as_str());
    }
    query.execute(c).await?;
    Ok(())
}
pub async fn update(
    c: &mut SqliteConnection,
    t: &str,
    data: &Value,
    columns: &[(&str, Value)],
) -> Result<()> {
    let mut columns = columns.to_vec();
    if let Some(key) = normalized_key(t) {
        columns.push((
            "normalized_text",
            json!(data[key].as_str().unwrap_or("").to_lowercase()),
        ));
    }
    let assignments = columns
        .iter()
        .map(|(k, _)| format!(",{k}=?"))
        .collect::<String>();
    let sql = format!("UPDATE {t} SET data=?{assignments} WHERE id=?");
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(data.to_string());
    for (_, value) in &columns {
        q = q.bind(value.as_str());
    }
    q.bind(data["id"].as_str()).execute(c).await?;
    Ok(())
}
pub async fn hydrate(
    c: &mut SqliteConnection,
    t: &str,
    mut data: Value,
    detail: bool,
) -> Result<Value> {
    let id = data["id"]
        .as_str()
        .ok_or_else(ApiError::internal)?
        .to_owned();
    match t {
        "vacancy" => {
            let company = get(
                c,
                "company",
                data["company_id"].as_str().ok_or_else(ApiError::internal)?,
            )
            .await?;
            data["company"] = json!({"id":company["id"],"name":company["name"]});
            let refs = sqlx::query_scalar::<_, String>(
                "SELECT data FROM vacancy_source_reference WHERE vacancy_id=? ORDER BY ingest_seq",
            )
            .bind(&id)
            .fetch_all(&mut *c)
            .await?;
            data["source_count"] = json!(refs.len());
            if detail {
                data["source_references"] = json!(
                    refs.iter()
                        .map(|s| serde_json::from_str::<Value>(s))
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|_| ApiError::internal())?
                );
            }
        }
        "contact" => {
            let roles=sqlx::query_scalar::<_,String>("SELECT r.data FROM contact_role r JOIN contact_role_assignment a ON a.role_id=r.id WHERE a.contact_id=? ORDER BY r.code").bind(&id).fetch_all(&mut *c).await?;
            let roles = roles
                .iter()
                .map(|s| serde_json::from_str::<Value>(s))
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|_| ApiError::internal())?;
            data["role_ids"] = json!(roles.iter().map(|r| r["id"].clone()).collect::<Vec<_>>());
            data["roles"] = json!(roles);
        }
        "conversation" if detail => {
            let rows=sqlx::query_scalar::<_,String>("SELECT data FROM conversation_participant WHERE conversation_id=? ORDER BY ingest_seq").bind(&id).fetch_all(c).await?;
            data["participants"] = json!(
                rows.iter()
                    .map(|s| serde_json::from_str::<Value>(s))
                    .collect::<std::result::Result<Vec<_>, _>>()
                    .map_err(|_| ApiError::internal())?
            );
        }
        _ => {}
    }
    Ok(data)
}
#[derive(Serialize, Deserialize)]
struct Cursor {
    scope: String,
    filters: BTreeMap<String, String>,
    high: i64,
    sort: String,
    id: String,
}
fn query_error(code: &'static str) -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        code,
        "Invalid or incompatible list query. Reset pagination.",
    )
}

pub async fn read(c: &mut SqliteConnection, parts: &[&str], query: &str) -> Result<Value> {
    if parts.len() == 2 {
        let t = table(parts[0])?;
        let data = get(c, t, parts[1]).await?;
        return Ok(json!({"data":hydrate(c,t,data,true).await?}));
    }
    let (t, parent, asc, sort) = match parts {
        [resource] => (table(resource)?, None, false, "created_at"),
        ["vacancies", id, "discoveries"] => {
            get(c, "vacancy", id).await?;
            (
                "vacancy_discovery",
                Some(("vacancy_id", *id)),
                false,
                "observed_at",
            )
        }
        ["vacancies", id, "status-history"] => {
            get(c, "vacancy", id).await?;
            (
                "vacancy_status_history",
                Some(("vacancy_id", *id)),
                false,
                "_sequence",
            )
        }
        ["conversations", id, "messages"] => {
            get(c, "conversation", id).await?;
            (
                "message",
                Some(("conversation_id", *id)),
                true,
                "occurred_at",
            )
        }
        _ => return Err(ApiError::missing()),
    };
    let mut args = BTreeMap::new();
    for (k, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if args.insert(k.to_string(), value.to_string()).is_some() {
            return Err(query_error("invalid_query"));
        }
    }
    let limit = args
        .remove("limit")
        .map(|s| s.parse::<usize>())
        .transpose()
        .map_err(|_| query_error("invalid_query"))?
        .unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(query_error("invalid_query"));
    }
    let cursor = args.remove("cursor");
    let scope = parts.join("/");
    let allowed: &[&str] = match t {
        "company" => &["q"],
        "contact" => &["q", "company_id"],
        "search_configuration" => &["enabled"],
        "vacancy" => &[
            "q",
            "status",
            "company_id",
            "source_id",
            "search_configuration_id",
        ],
        "conversation" => &["company_id", "vacancy_id", "channel_id"],
        _ => &[],
    };
    if args.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(query_error("invalid_query"));
    }
    for (k, value) in &args {
        if (k.ends_with("_id") && uuid::Uuid::parse_str(value).is_err())
            || (k == "q" && (value.trim().is_empty() || value.chars().count() > 200))
            || (k == "status" && !v::STATUSES.contains(&value.as_str()))
            || (k == "enabled" && !["true", "false"].contains(&value.as_str()))
        {
            return Err(query_error("invalid_query"));
        }
    }
    let token = cursor
        .map(|s| {
            URL_SAFE_NO_PAD
                .decode(s)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Cursor>(&bytes).ok())
                .ok_or_else(|| query_error("invalid_cursor"))
        })
        .transpose()?;
    if token
        .as_ref()
        .is_some_and(|x| x.scope != scope || x.filters != args || x.high < 0)
    {
        return Err(query_error("invalid_cursor"));
    }
    if sort == "_sequence"
        && token.as_ref().is_some_and(|x| {
            !x.sort
                .parse::<i64>()
                .is_ok_and(|seq| seq > 0 && seq <= x.high)
        })
    {
        return Err(query_error("invalid_cursor"));
    }
    let high = if let Some(x) = &token {
        x.high
    } else {
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "SELECT COALESCE(MAX(ingest_seq),0) FROM {t}"
        )))
        .fetch_one(&mut *c)
        .await?
    };
    let mut sql = format!("SELECT data,id,ingest_seq FROM {t} r WHERE ingest_seq<=?");
    let mut binds = Vec::new();
    if let Some((column, id)) = parent {
        sql.push_str(&format!(" AND {column}=?"));
        binds.push(id.to_owned());
    }
    for (k, value) in &args {
        match k.as_str() {
            "q" => {
                sql.push_str(" AND instr(r.normalized_text,?)>0");
                binds.push(value.to_lowercase());
            }
            "enabled" => {
                sql.push_str(" AND json_extract(data,'$.enabled')=CAST(? AS INTEGER)");
                binds.push(if value == "true" {
                    "1".into()
                } else {
                    "0".into()
                });
            }
            "source_id" => {
                sql.push_str(" AND EXISTS(SELECT 1 FROM vacancy_source_reference s WHERE s.vacancy_id=r.id AND s.source_id=?)");
                binds.push(value.clone());
            }
            "search_configuration_id" => {
                sql.push_str(" AND EXISTS(SELECT 1 FROM vacancy_discovery d WHERE d.vacancy_id=r.id AND d.search_configuration_id=?)");
                binds.push(value.clone());
            }
            "status" => {
                sql.push_str(" AND json_extract(data,'$.status')=?");
                binds.push(value.clone());
            }
            _ => {
                sql.push_str(&format!(" AND {k}=?"));
                binds.push(value.clone());
            }
        }
    }
    let direction = if asc { "ASC" } else { "DESC" };
    let comparison = if asc { ">" } else { "<" };
    let sort_expression = if sort == "_sequence" {
        "r.ingest_seq".to_owned()
    } else {
        format!("json_extract(data,'$.{sort}')")
    };
    if let Some(x) = &token {
        let position = if sort == "_sequence" {
            "CAST(? AS INTEGER)"
        } else {
            "?"
        };
        sql.push_str(&format!(
            " AND ({sort_expression},id){comparison}({position},?)"
        ));
        binds.push(x.sort.clone());
        binds.push(x.id.clone());
    }
    sql.push_str(&format!(
        " ORDER BY {sort_expression} {direction},id {direction} LIMIT ?"
    ));
    let mut q = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(high);
    for b in binds {
        q = q.bind(b);
    }
    let rows = q.bind((limit + 1) as i64).fetch_all(&mut *c).await?;
    let more = rows.len() > limit;
    let mut items = Vec::new();
    let mut last_sequence = 0_i64;
    for row in rows.into_iter().take(limit) {
        last_sequence = row.get("ingest_seq");
        let data: Value =
            serde_json::from_str(row.get::<&str, _>("data")).map_err(|_| ApiError::internal())?;
        items.push(hydrate(c, t, data, false).await?);
    }
    let next = if more {
        let last = items.last().ok_or_else(ApiError::internal)?;
        Some(
            URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&Cursor {
                    scope,
                    filters: args,
                    high,
                    sort: if sort == "_sequence" {
                        last_sequence.to_string()
                    } else {
                        last[sort].as_str().ok_or_else(ApiError::internal)?.into()
                    },
                    id: last["id"].as_str().ok_or_else(ApiError::internal)?.into(),
                })
                .map_err(|_| ApiError::internal())?,
            ),
        )
    } else {
        None
    };
    Ok(json!({"items":items,"next_cursor":next}))
}
pub async fn backfill_search(c: &mut SqliteConnection) -> Result<()> {
    for t in ["company", "contact", "vacancy"] {
        let sql = format!("SELECT id,data FROM {t} WHERE normalized_text=''");
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .fetch_all(&mut *c)
            .await?;
        for row in rows {
            let data: Value =
                serde_json::from_str(row.get("data")).map_err(|_| ApiError::internal())?;
            update(c, t, &data, &[]).await?;
        }
    }
    Ok(())
}
