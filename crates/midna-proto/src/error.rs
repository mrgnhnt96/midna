//! JSON-RPC error codes (see ARCHITECTURE.md "Errors").
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const UNKNOWN_METHOD: i64 = -32601;
pub const BAD_PARAMS: i64 = -32602;
pub const INTERNAL: i64 = -32603;
pub const REFUSED: i64 = 1;
pub const HUMAN_ONLY: i64 = 2;
pub const NOT_FOUND: i64 = 3;
pub const CONFLICT: i64 = 4;
pub const NOT_IMPLEMENTED: i64 = 5;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into(), data: None }
    }
    pub fn with_data(mut self, data: Value) -> RpcError {
        self.data = Some(data);
        self
    }
    pub fn bad_params(m: impl Into<String>) -> RpcError {
        RpcError::new(BAD_PARAMS, m)
    }
    pub fn not_found(m: impl Into<String>) -> RpcError {
        RpcError::new(NOT_FOUND, m)
    }
    pub fn refused(m: impl Into<String>) -> RpcError {
        RpcError::new(REFUSED, m)
    }
    pub fn human_only(m: impl Into<String>) -> RpcError {
        RpcError::new(HUMAN_ONLY, m)
    }
    pub fn conflict(m: impl Into<String>) -> RpcError {
        RpcError::new(CONFLICT, m)
    }
    pub fn internal(m: impl Into<String>) -> RpcError {
        RpcError::new(INTERNAL, m)
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}
impl std::error::Error for RpcError {}
