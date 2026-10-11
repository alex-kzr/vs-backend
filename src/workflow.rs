//! Transactional transport for explicitly initiated CV and external-agent workflows.
use crate::{
    cvs,
    error::{ApiError, Result},
    matching, storage as s,
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use sqlx::{Row, SqliteConnection};
pub fn handles(parts: &[&str]) -> bool {
    matches!(parts.first(), Some(&"cvs" | &"match-runs"))
}
pub async fn post(
    c: &mut SqliteConnection,
    parts: &[&str],
    key: &str,
    input: Value,
) -> Result<(StatusCode, Value)> {
    if parts == ["cvs", "import-preview"] {
        return Ok((
            StatusCode::OK,
            json!({"data":cvs::normalize(&input,false,false)?}),
        ));
    }
    if parts == ["match-runs", "preview"] {
        return Ok((
            StatusCode::OK,
            json!({"data":matching::preview(c,&matching::normalize_selection(&input,false)?).await?}),
        ));
    }
    let normalized = match parts {
        ["cvs"] => cvs::normalize(&input, true, false)?,
        ["cvs", _, "revisions"] => cvs::normalize(&input, true, true)?,
        ["cvs", _, "delete" | "restore"] => cvs::normalize_lifecycle(&input)?,
        _ => matching::normalize(parts, &input)?,
    };
    let operation = format!("workflow:{}", parts.join("/"));
    let receipt=sqlx::query("SELECT request_json,result_json FROM request_receipt WHERE operation=? AND target='' AND key=?")
  .bind(&operation).bind(key).fetch_optional(&mut *c).await?;
    if let Some(row) = receipt {
        let previous: Value =
            serde_json::from_str(row.get("request_json")).map_err(|_| ApiError::internal())?;
        if previous != normalized {
            // Existing receipts remain byte-for-byte intact across the additive source migration.
            let compatible = if matches!(parts, ["cvs"] | ["cvs", _, "revisions"])
                && previous.get("source_code").is_none()
            {
                let mut legacy = previous.clone();
                legacy["confirmed"] = json!(true);
                cvs::normalize(&legacy, true, parts.len() == 3).ok()
            } else {
                None
            };
            if compatible.as_ref() != Some(&normalized) {
                return Err(ApiError::conflict("idempotency_conflict"));
            }
        }
        let result: Value =
            serde_json::from_str(row.get("result_json")).map_err(|_| ApiError::internal())?;
        return Ok((StatusCode::OK, json!({"data":result,"deduplicated":true})));
    }
    let result = match parts {
        ["cvs"] => cvs::save(c, None, &normalized).await?,
        ["cvs", id, "revisions"] => cvs::save(c, Some(id), &normalized).await?,
        ["cvs", id, action @ ("delete" | "restore")] => {
            cvs::change_lifecycle(c, id, *action == "delete", &normalized).await?
        }
        _ => matching::write(c, parts, &normalized).await?,
    };
    sqlx::query("INSERT INTO request_receipt(operation,target,key,request_json,result_json) VALUES(?,'',?,?,?)")
  .bind(operation).bind(key).bind(normalized.to_string()).bind(result.to_string()).execute(c).await?;
    let status = if parts.len() == 1 || matches!(parts, ["cvs", _, "revisions"]) {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, json!({"data":result,"deduplicated":false})))
}
pub async fn read(c: &mut SqliteConnection, parts: &[&str], query: &str) -> Result<Value> {
    if matches!(parts, ["cvs", _, "revisions"]) {
        return s::read(c, parts, query).await;
    }
    if parts.len() > 2 {
        if !query.is_empty() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_query",
                "Вложенные CV/agent endpoints не принимают query параметры.",
            ));
        }
        return if parts[0] == "cvs" {
            cvs::read(c, parts).await
        } else {
            matching::read(c, parts).await
        };
    }
    s::read(c, parts, query).await
}
