use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{IndexOptions, QueryRequest};

pub const PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub protocol_version: u32,
    pub request: Request,
}

impl RequestEnvelope {
    pub fn new(request: Request) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Request {
    Ping,
    Index {
        root: PathBuf,
        options: IndexOptions,
    },
    Notify {
        paths: Vec<PathBuf>,
        options: IndexOptions,
    },
    Search {
        query: String,
        limit: usize,
        roots: Vec<PathBuf>,
        kinds: Vec<String>,
        path_prefix: Option<String>,
        #[serde(default)]
        context_path: Option<PathBuf>,
    },
    Inspect {
        path: PathBuf,
        start_line: usize,
        max_lines: usize,
        max_chars: usize,
    },
    Query {
        request: QueryRequest,
    },
    Status,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol_version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}

impl ResponseEnvelope {
    pub fn success<T: Serialize>(value: T) -> serde_json::Result<Self> {
        Ok(Self {
            protocol_version: PROTOCOL_VERSION,
            ok: true,
            result: Some(serde_json::to_value(value)?),
            error: None,
        })
    }

    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            ok: false,
            result: None,
            error: Some(ApiError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    pub fn into_result(self) -> anyhow::Result<Value> {
        if self.protocol_version != PROTOCOL_VERSION {
            anyhow::bail!(
                "AWI protocol mismatch: client={}, server={}",
                PROTOCOL_VERSION,
                self.protocol_version
            );
        }
        if self.ok {
            Ok(self.result.unwrap_or(Value::Null))
        } else {
            let error = self.error.unwrap_or(ApiError {
                code: "unknown".to_owned(),
                message: "daemon returned an unspecified error".to_owned(),
            });
            anyhow::bail!("AWI daemon {}: {}", error.code, error.message);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_envelope_has_stable_method_name() {
        let encoded = serde_json::to_string(&RequestEnvelope::new(Request::Search {
            query: "campaign score".to_owned(),
            limit: 5,
            roots: Vec::new(),
            kinds: Vec::new(),
            path_prefix: None,
            context_path: None,
        }))
        .unwrap();
        assert!(encoded.contains("\"method\":\"search\""));
        assert!(encoded.contains("\"protocol_version\":3"));
    }
}
