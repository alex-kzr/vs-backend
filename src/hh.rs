//! Explicit, bounded access to the official HH API. No scheduler or account actions.
mod mapping;
pub mod query;
#[cfg(test)]
mod tests;

use crate::{
    error::{ApiError, Result},
    validation as v,
};
use axum::http::{HeaderValue, StatusCode};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MAX_BODY: usize = 512 * 1024;
enum Access {
    Blocked(&'static str),
    Ready {
        client: reqwest::Client,
        authorization: Option<HeaderValue>,
        user_agent: HeaderValue,
        endpoint: url::Url,
    },
}
struct Inner {
    access: Access,
    permit: Semaphore,
    next: Mutex<Instant>,
    interval: Duration,
    requests: Mutex<HashMap<Uuid, (Instant, CancellationToken)>>,
}
#[derive(Clone)]
pub struct HhClient {
    inner: Arc<Inner>,
}
impl Default for HhClient {
    fn default() -> Self {
        Self::blocked(
            "Register and obtain access for your HH application, accept the applicable agreements, then configure local HH access. No live requests are enabled.",
        )
    }
}
fn error(status: StatusCode, code: &'static str, message: &str) -> ApiError {
    ApiError::new(status, code, message)
}
fn cancelled() -> ApiError {
    error(
        StatusCode::CONFLICT,
        "hh_cancelled",
        "HH preview was cancelled. No records were imported.",
    )
}
impl HhClient {
    fn blocked(reason: &'static str) -> Self {
        // Constant production URL; no user-supplied remote host or secret-bearing URL.
        Self {
            inner: Arc::new(Inner {
                access: Access::Blocked(reason),
                permit: Semaphore::new(1),
                next: Mutex::new(Instant::now()),
                interval: Duration::from_secs(1),
                requests: Mutex::new(HashMap::new()),
            }),
        }
    }
    pub fn from_env() -> Self {
        if std::env::var("HH_API_ACCESS_CONFIRMED").as_deref() != Ok("1") {
            return Self::default();
        }
        let Ok(user_agent) = std::env::var("HH_USER_AGENT") else {
            return Self::blocked(
                "Set HH_USER_AGENT locally with your registered application identity/version and real developer contact.",
            );
        };
        if user_agent.len() > 500
            || !user_agent.contains('/')
            || !user_agent.contains('(')
            || !user_agent.contains('@')
            || !user_agent.ends_with(')')
        {
            return Self::blocked(
                "HH_USER_AGENT must identify the registered app/version and developer email, as required by HH.",
            );
        }
        let token = std::env::var("HH_API_TOKEN").ok();
        if token.as_ref().is_some_and(|value| {
            value.trim().is_empty()
                || value.len() > 4096
                || value.bytes().any(|c| c.is_ascii_whitespace())
        }) {
            return Self::blocked(
                "HH_API_TOKEN is invalid. Configure an authorized token locally, or omit it for an anonymous request.",
            );
        }
        Self::ready("https://api.hh.ru/",&user_agent,token.as_deref(),Duration::from_secs(15),Duration::from_secs(1),true).unwrap_or_else(|_|Self::blocked("HH client configuration is invalid. Inspect local settings without sharing credentials."))
    }
    fn ready(
        endpoint: &str,
        ua: &str,
        token: Option<&str>,
        timeout: Duration,
        interval: Duration,
        https_only: bool,
    ) -> Result<Self> {
        let mut user_agent = HeaderValue::from_str(ua).map_err(|_| ApiError::internal())?;
        user_agent.set_sensitive(true);
        let authorization = token
            .map(|token| {
                let mut header = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| ApiError::internal())?;
                header.set_sensitive(true);
                Ok::<_, ApiError>(header)
            })
            .transpose()?;
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .connect_timeout(timeout.min(Duration::from_secs(5)))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .https_only(https_only)
            .no_proxy()
            .build()
            .map_err(|_| ApiError::internal())?;
        let endpoint = url::Url::parse(endpoint).map_err(|_| ApiError::internal())?;
        Ok(Self {
            inner: Arc::new(Inner {
                access: Access::Ready {
                    client,
                    authorization,
                    user_agent,
                    endpoint,
                },
                permit: Semaphore::new(1),
                next: Mutex::new(Instant::now()),
                interval,
                requests: Mutex::new(HashMap::new()),
            }),
        })
    }
    pub fn status(&self) -> Value {
        let (ready, message) = match &self.inner.access {
            Access::Blocked(reason) => (false, *reason),
            Access::Ready { .. } => (
                true,
                "HH access is configured locally. The next requested page may still be denied by HH.",
            ),
        };
        json!({"adapter":"hh","configured":ready,"message":message,"registration_url":"https://dev.hh.ru/",
            "terms_url":"https://dev.hh.ru/admin/developer_agreement","agreement_url":"https://hh.ru/account/agreement",
            "max_per_page":50,"result_limit":query::MAX_RESULTS,"timeout_seconds":15,"automatic_requests":false})
    }
    fn token(&self, id: Uuid) -> Result<CancellationToken> {
        let mut requests = self
            .inner
            .requests
            .lock()
            .map_err(|_| ApiError::internal())?;
        requests.retain(|_, (time, _)| time.elapsed() < Duration::from_secs(120));
        if !requests.contains_key(&id) && requests.len() >= 64 {
            return Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "hh_busy",
                "Too many pending HH requests. Try later.",
            )
            .with_retry_after(2));
        }
        Ok(requests
            .entry(id)
            .or_insert_with(|| (Instant::now(), CancellationToken::new()))
            .1
            .clone())
    }
    pub fn cancel(&self, id: &str) -> Result<Value> {
        let id = Uuid::parse_str(id).map_err(|_| query::invalid())?;
        self.token(id)?.cancel();
        Ok(json!({"cancelled":true,"request_id":id}))
    }
    async fn fetch(&self, path: &str, params: &[(String, String)], id: Uuid) -> Result<Value> {
        let Access::Ready {
            client,
            authorization,
            user_agent,
            endpoint,
        } = &self.inner.access
        else {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "hh_setup_required",
                self.status()["message"]
                    .as_str()
                    .unwrap_or("Configure authorized HH access."),
            ));
        };
        let cancellation = self.token(id)?;
        if cancellation.is_cancelled() {
            return Err(cancelled());
        }
        let _permit = self.inner.permit.try_acquire().map_err(|_| {
            error(
                StatusCode::TOO_MANY_REQUESTS,
                "hh_busy",
                "Another HH request is active. Wait or cancel it.",
            )
            .with_retry_after(1)
        })?;
        {
            let mut next = self.inner.next.lock().map_err(|_| ApiError::internal())?;
            let now = Instant::now();
            if *next > now {
                return Err(error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "hh_rate_limited",
                    "HH requests are paced or cooling down. Wait before requesting another page.",
                )
                .with_retry_after((*next - now).as_secs().saturating_add(1)));
            }
            *next = now + self.inner.interval;
        }
        let mut url = endpoint.join(path).map_err(|_| ApiError::internal())?;
        url.query_pairs_mut()
            .extend_pairs(params.iter().map(|(a, b)| (a.as_str(), b.as_str())));
        let future = async {
            let mut builder = client
                .get(url)
                .header("HH-User-Agent", user_agent.clone())
                .header("Accept", "application/json");
            if let Some(authorization) = authorization {
                builder = builder.header("Authorization", authorization.clone());
            }
            let mut response = builder.send().await.map_err(network_error)?;
            let status = response.status();
            let retry = retry_after(
                response
                    .headers()
                    .get("retry-after")
                    .and_then(|x| x.to_str().ok()),
            );
            if status == StatusCode::TOO_MANY_REQUESTS {
                let mut next = self.inner.next.lock().map_err(|_| ApiError::internal())?;
                *next = Instant::now()
                    .checked_add(Duration::from_secs(retry))
                    .unwrap_or(*next);
                return Err(error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "hh_rate_limited",
                    "HH returned a rate limit. Wait for Retry-After; no automatic retry is made.",
                )
                .with_retry_after(retry));
            }
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                return Err(error(
                    StatusCode::FORBIDDEN,
                    "hh_access_denied",
                    "HH denied access or requires verification. Check your registered app/token or complete HH verification yourself. No bypass or automatic retry is attempted.",
                ));
            }
            if status == StatusCode::BAD_REQUEST {
                return Err(error(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "hh_invalid_criteria",
                    "HH rejected the search parameters. Review your query and regions.",
                ));
            }
            if !status.is_success() {
                return Err(error(
                    StatusCode::BAD_GATEWAY,
                    "hh_unavailable",
                    "HH is unavailable or returned an unexpected status. Retry manually later.",
                ));
            }
            if response
                .content_length()
                .is_some_and(|n| n > MAX_BODY as u64)
            {
                return Err(error(
                    StatusCode::BAD_GATEWAY,
                    "hh_response_too_large",
                    "HH response exceeds the bounded preview size.",
                ));
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(network_error)? {
                if bytes.len() + chunk.len() > MAX_BODY {
                    return Err(error(
                        StatusCode::BAD_GATEWAY,
                        "hh_response_too_large",
                        "HH response exceeds the bounded preview size.",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| {
                error(
                    StatusCode::BAD_GATEWAY,
                    "hh_invalid_response",
                    "HH returned invalid JSON. No records were imported.",
                )
            })
        };
        tokio::select! {biased;_=cancellation.cancelled()=>Err(cancelled()),result=future=>result}
    }
    pub async fn preview(&self, configuration: &Value, page: u64, id: Uuid) -> Result<Value> {
        let (hh, params) = query::parameters(configuration, page)?;
        let response = self.fetch("vacancies", &params, id).await?;
        mapping::preview(response, configuration, &hh, page, &v::now())
    }
    pub async fn areas(&self, raw: &str) -> Result<Value> {
        let args = query::arguments(raw, &["text", "request_id"])?;
        let text = args
            .get("text")
            .filter(|x| (2..=200).contains(&x.chars().count()))
            .ok_or_else(query::invalid)?;
        let id = args
            .get("request_id")
            .map(|x| Uuid::parse_str(x))
            .transpose()
            .map_err(|_| query::invalid())?
            .unwrap_or_else(Uuid::new_v4);
        let response = self
            .fetch(
                "suggests/areas",
                &[
                    ("text".into(), text.clone()),
                    ("host".into(), "hh.ru".into()),
                ],
                id,
            )
            .await?;
        let items = response["items"].as_array().ok_or_else(|| {
            error(
                StatusCode::BAD_GATEWAY,
                "hh_invalid_response",
                "HH region suggestions were invalid.",
            )
        })?;
        Ok(json!({"items":items.iter().take(20).filter_map(|x| {
            let id=x["id"].as_str().filter(|s|!s.is_empty()&&s.len()<=20&&s.bytes().all(|c|c.is_ascii_digit()))?;
            let name=x["text"].as_str().filter(|s|!s.trim().is_empty()&&s.chars().count()<=300)?;
            Some(json!({"id":id,"name":name}))
        }).collect::<Vec<_>>()}))
    }
}
fn network_error(e: reqwest::Error) -> ApiError {
    if e.is_timeout() {
        error(
            StatusCode::GATEWAY_TIMEOUT,
            "hh_timeout",
            "HH timed out. No records were imported; retry manually or reduce the page size.",
        )
    } else {
        error(
            StatusCode::BAD_GATEWAY,
            "hh_network_error",
            "Could not reach HH over HTTPS. No records were imported. Check your connection and retry manually.",
        )
    }
}
fn retry_after(value: Option<&str>) -> u64 {
    value
        .and_then(|v| {
            v.parse::<u64>().ok().or_else(|| {
                chrono::DateTime::parse_from_rfc2822(v)
                    .ok()
                    .map(|d| (d.timestamp() - chrono::Utc::now().timestamp()).max(1) as u64)
            })
        })
        .unwrap_or(60)
        .max(1)
}
pub fn source_payload(value: &Value) -> Result<Value> {
    mapping::source_payload(value)
}
