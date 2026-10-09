use axum::{
    Json,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub fields: Value,
}
impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            fields: json!({}),
        }
    }
    pub fn field(field: &str, message: &str) -> Self {
        let mut e = Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            "Some fields are invalid.",
        );
        e.fields = json!({field:[message]});
        e
    }
    pub fn conflict(code: &'static str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            code,
            "This identity is already used with different input. Inspect the existing record.",
        )
    }
    pub fn missing() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "Record not found.")
    }
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "The operation could not be completed.",
        )
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(ref e) = error {
            if e.is_unique_violation() {
                return Self::conflict("unique_conflict");
            }
            if matches!(e.code().as_deref(), Some("5" | "6" | "261" | "262" | "517")) {
                return Self::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "database_busy",
                    "Database is busy. Retry with the same key.",
                );
            }
            if e.is_foreign_key_violation() || e.is_check_violation() {
                return Self::field("relationship", "Invalid relationship or value.");
            }
        }
        Self::internal()
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let busy = self.status == StatusCode::SERVICE_UNAVAILABLE;
        let mut response = (self.status, Json(json!({"error":{
            "code":self.code,"message":self.message,"fields":self.fields,"request_id":Uuid::new_v4().to_string()
        }}))).into_response();
        if busy {
            response.headers_mut().insert(
                header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
        }
        response
    }
}
pub type Result<T> = std::result::Result<T, ApiError>;
