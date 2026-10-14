//! ZeroTier Central API v1: https://docs.zerotier.com/api/central/v1
//!
//! Only what the integration uses: the account's networks, their members and
//! authorizing a member. The token goes in `Authorization: token <token>`.

use std::time::Duration;

use serde_json::{json, Value};

use crate::model::{Member, Network};

pub const BASE_URL: &str = "https://api.zerotier.com/api/v1";
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub enum ApiError {
    /// The token was rejected (401 or 403).
    Auth,
    Other(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Auth => write!(f, "ZeroTier afviste API-tokenet"),
            ApiError::Other(m) => write!(f, "{m}"),
        }
    }
}

pub struct Client {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(base: &str, token: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("thehome-zerotier/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Self { base: base.trim_end_matches('/').to_string(), token: token.to_string(), http }
    }

    async fn send(&self, req: reqwest::RequestBuilder, what: &str) -> Result<Value, ApiError> {
        let res = req
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .map_err(|e| ApiError::Other(format!("{what}: {}", without_url(&e))))?;
        let status = res.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ApiError::Auth);
        }
        if !status.is_success() {
            return Err(ApiError::Other(format!("{what}: HTTP {status}")));
        }
        res.json().await.map_err(|e| ApiError::Other(format!("{what}: {}", without_url(&e))))
    }

    async fn get(&self, path: &str) -> Result<Value, ApiError> {
        self.send(self.http.get(format!("{}{path}", self.base)), path).await
    }

    /// The logged-in user's id, if the API returns one.
    pub async fn user_id(&self) -> Result<Option<String>, ApiError> {
        let v = self.get("/status").await?;
        Ok(v["user"]["id"].as_str().filter(|s| !s.is_empty()).map(String::from))
    }

    pub async fn networks(&self) -> Result<Vec<Network>, ApiError> {
        let v = self.get("/network").await?;
        serde_json::from_value(v).map_err(|e| ApiError::Other(format!("netværk: {e}")))
    }

    pub async fn members(&self, network_id: &str) -> Result<Vec<Member>, ApiError> {
        let v = self.get(&format!("/network/{network_id}/member")).await?;
        serde_json::from_value(v).map_err(|e| ApiError::Other(format!("medlemmer: {e}")))
    }

    /// Authorize or deauthorize a member. Returns the updated member.
    pub async fn set_authorized(&self, network_id: &str, node_id: &str, authorized: bool) -> Result<Member, ApiError> {
        let path = format!("/network/{network_id}/member/{node_id}");
        let req = self.http.post(format!("{}{path}", self.base)).json(&json!({ "config": { "authorized": authorized } }));
        let v = self.send(req, &path).await?;
        serde_json::from_value(v).map_err(|e| ApiError::Other(format!("medlem: {e}")))
    }
}

/// reqwest errors include the URL; keep the log short.
fn without_url(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(url) = e.url() {
        s = s.replace(&format!(" for url ({url})"), "");
    }
    s
}
