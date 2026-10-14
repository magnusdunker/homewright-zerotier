//! Central's network and member objects, and the values the entities show.
//! Fields are optional with defaults, since Central leaves some out.

use serde::Deserialize;

/// Seen by the controller within this long counts as online. my.zerotier.com
/// shows a member as online for about 3 minutes after its last check-in.
pub const ONLINE_WINDOW_MS: i64 = 5 * 60 * 1000;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Network {
    pub id: String,
    pub description: String,
    pub config: NetworkConfig,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct NetworkConfig {
    pub name: String,
    pub private: bool,
}

impl Network {
    pub fn name(&self) -> &str {
        if self.config.name.trim().is_empty() {
            &self.id
        } else {
            self.config.name.trim()
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Member {
    pub network_id: String,
    pub node_id: String,
    pub name: String,
    pub description: String,
    pub hidden: bool,
    pub config: MemberConfig,
    pub last_online: Option<i64>,
    pub last_seen: Option<i64>,
    pub physical_address: Option<String>,
    pub client_version: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct MemberConfig {
    pub id: String,
    pub authorized: bool,
    pub ip_assignments: Vec<String>,
    pub v_major: Option<i64>,
    pub v_minor: Option<i64>,
    pub v_rev: Option<i64>,
}

impl Member {
    /// The node's 10-digit address.
    pub fn node(&self) -> &str {
        if self.node_id.is_empty() {
            &self.config.id
        } else {
            &self.node_id
        }
    }

    pub fn display_name(&self) -> String {
        match self.name.trim() {
            "" => self.node().to_string(),
            n => n.to_string(),
        }
    }

    /// Last contact with the controller in ms since epoch. Central documents
    /// both fields as ephemeral and may reset them to 0.
    pub fn last_contact(&self) -> Option<i64> {
        [self.last_seen, self.last_online].into_iter().flatten().filter(|t| *t > 0).max()
    }

    pub fn is_online(&self, now_ms: i64) -> bool {
        self.last_contact().is_some_and(|t| now_ms - t <= ONLINE_WINDOW_MS)
    }

    pub fn version(&self) -> Option<String> {
        if let Some(v) = self.client_version.as_deref().map(str::trim).filter(|v| !v.is_empty() && *v != "unknown") {
            return Some(v.trim_start_matches('v').to_string());
        }
        match (self.config.v_major, self.config.v_minor, self.config.v_rev) {
            (Some(a), Some(b), Some(c)) if a >= 0 && b >= 0 && c >= 0 => Some(format!("{a}.{b}.{c}")),
            _ => None,
        }
    }

    pub fn ip(&self) -> Option<&str> {
        self.config.ip_assignments.first().map(String::as_str)
    }

    pub fn physical_ip(&self) -> Option<&str> {
        self.physical_address.as_deref().filter(|a| !a.is_empty())
    }
}

/// ISO 8601 for a timestamp sensor.
pub fn iso_time(ms: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(ms).map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

/// (authorized and online, authorized) among `members`. Members waiting to
/// be authorized also check in, so they are not counted as online.
pub fn counts(members: &[Member], now_ms: i64) -> (usize, usize) {
    let online = members.iter().filter(|m| m.config.authorized && m.is_online(now_ms)).count();
    let authorized = members.iter().filter(|m| m.config.authorized).count();
    (online, authorized)
}
