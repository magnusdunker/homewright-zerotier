//! ZeroTier Central has two APIs, and a token only works with one of them:
//!
//! - New Central, https://central.zerotier.com/api/v2, with
//!   `Authorization: Bearer <key>`. Keys belong to service accounts
//!   (Organization → Service Accounts).
//!   Spec: https://docs.zerotier.com/openapi/central/v2.yaml
//! - Legacy Central, https://api.zerotier.com/api/v1, with
//!   `Authorization: token <token>`. Tokens are made under Account → API
//!   Access Tokens. Spec: https://docs.zerotier.com/openapi/central/v1.json
//!
//! [`Client::detect`] tries both. Both are mapped to the same
//! [`Network`] and [`Member`].

use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::model::{Member, MemberConfig, Network, NetworkConfig};

pub const V1_URL: &str = "https://api.zerotier.com/api/v1";
pub const V2_URL: &str = "https://central.zerotier.com/api/v2";
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Version {
    V1,
    V2,
}

impl Version {
    pub fn as_str(self) -> &'static str {
        match self {
            Version::V1 => "v1",
            Version::V2 => "v2",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "v1" => Some(Version::V1),
            "v2" => Some(Version::V2),
            _ => None,
        }
    }
}

pub struct Client {
    version: Version,
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(version: Version, base: &str, token: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(concat!("thehome-zerotier/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("http client");
        Self { version, base: base.trim_end_matches('/').to_string(), token: token.trim().to_string(), http }
    }

    /// Find the API the token belongs to: New Central first, then Legacy.
    /// Returns the client and the account's networks.
    pub async fn detect(v1_base: &str, v2_base: &str, token: &str) -> Result<(Client, Vec<Network>), ApiError> {
        let v2 = Client::new(Version::V2, v2_base, token);
        let v2_err = match v2.networks().await {
            Ok(n) => return Ok((v2, n)),
            Err(e) => e,
        };
        let v1 = Client::new(Version::V1, v1_base, token);
        match (v1.networks().await, v2_err) {
            (Ok(n), _) => Ok((v1, n)),
            // a network error says more than a rejection by the other API
            (Err(ApiError::Auth), e) | (Err(e), ApiError::Auth) => Err(e),
            (Err(_), e) => Err(e),
        }
    }

    pub fn version(&self) -> Version {
        self.version
    }

    async fn send(&self, req: reqwest::RequestBuilder, what: &str) -> Result<Value, ApiError> {
        let auth = match self.version {
            Version::V1 => format!("token {}", self.token),
            Version::V2 => format!("Bearer {}", self.token),
        };
        let res = req
            .header("Authorization", auth)
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

    /// An id for the account, if the API gives one (only Legacy Central).
    pub async fn user_id(&self) -> Result<Option<String>, ApiError> {
        if self.version == Version::V2 {
            return Ok(None);
        }
        let v = self.get("/status").await?;
        Ok(v["user"]["id"].as_str().filter(|s| !s.is_empty()).map(String::from))
    }

    pub async fn networks(&self) -> Result<Vec<Network>, ApiError> {
        let v = self.get("/network").await?;
        let parse = |e: serde_json::Error| ApiError::Other(format!("netværk: {e}"));
        match self.version {
            Version::V1 => serde_json::from_value(v).map_err(parse),
            Version::V2 => {
                let list: V2Networks = serde_json::from_value(v).map_err(parse)?;
                Ok(list.items.into_iter().map(V2Network::into_network).collect())
            }
        }
    }

    pub async fn members(&self, network_id: &str) -> Result<Vec<Member>, ApiError> {
        let v = self.get(&format!("/network/{network_id}/member")).await?;
        let parse = |e: serde_json::Error| ApiError::Other(format!("medlemmer: {e}"));
        match self.version {
            Version::V1 => serde_json::from_value(v).map_err(parse),
            Version::V2 => {
                let list: Vec<V2Member> = serde_json::from_value(v).map_err(parse)?;
                Ok(list.into_iter().map(V2Member::into_member).collect())
            }
        }
    }

    /// Authorize or deauthorize a member. Returns the updated member.
    pub async fn set_authorized(&self, network_id: &str, node_id: &str, authorized: bool) -> Result<Member, ApiError> {
        let parse = |e: serde_json::Error| ApiError::Other(format!("medlem: {e}"));
        match self.version {
            Version::V1 => {
                let path = format!("/network/{network_id}/member/{node_id}");
                let req = self.http.post(format!("{}{path}", self.base)).json(&json!({ "config": { "authorized": authorized } }));
                serde_json::from_value(self.send(req, &path).await?).map_err(parse)
            }
            Version::V2 => {
                let action = if authorized { "authorize" } else { "de-authorize" };
                let path = format!("/network/{network_id}/member/{node_id}/{action}");
                let v = self.send(self.http.post(format!("{}{path}", self.base)), &path).await?;
                serde_json::from_value::<V2Member>(v).map(V2Member::into_member).map_err(parse)
            }
        }
    }
}

#[derive(Deserialize)]
struct V2Networks {
    #[serde(default)]
    items: Vec<V2Network>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct V2Network {
    id: String,
    name: String,
    description: String,
}

impl V2Network {
    fn into_network(self) -> Network {
        // New Central does not say whether a network is private; members
        // always need authorizing there
        Network { config: NetworkConfig { name: self.name, private: true }, id: self.id, description: self.description }
    }
}

/// A member as New Central returns it.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct V2Member {
    device_id: String,
    network_id: String,
    name: String,
    description: String,
    status: String,
    ipv4_assignments: Vec<String>,
    ipv6_assignments: Vec<String>,
    agent_version: String,
    last_seen_ip: String,
    last_seen_time: Option<String>,
}

impl V2Member {
    pub fn into_member(self) -> Member {
        let mut ips = self.ipv4_assignments;
        ips.extend(self.ipv6_assignments);
        let seen = self
            .last_seen_time
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp_millis())
            .filter(|t| *t > 0);
        Member {
            network_id: self.network_id,
            node_id: self.device_id.clone(),
            name: self.name,
            description: self.description,
            hidden: false,
            config: MemberConfig { id: self.device_id, authorized: self.status == "authorized", ip_assignments: ips, ..Default::default() },
            last_online: None,
            last_seen: seen,
            physical_address: Some(self.last_seen_ip).filter(|s| !s.is_empty()),
            client_version: Some(self.agent_version).filter(|s| !s.is_empty()),
        }
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
