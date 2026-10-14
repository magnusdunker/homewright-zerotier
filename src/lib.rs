//! ZeroTier: networks and their members from ZeroTier Central, as an
//! external integration for TheHome.
//!
//! The user pastes an API token from my.zerotier.com and picks networks. Each
//! network becomes a device with member counts; each member becomes a device
//! connected through its network, with an online sensor, a switch that
//! authorizes it, last seen, ZeroTier IP and client version.

pub mod api;
pub mod model;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thehome_plugin::{async_trait, json, Device, Entity, Entry, Error, Flow, FlowReply, Host, Integration, Map, Value};

use api::{ApiError, Client};
use model::{Member, Network};

pub const DOMAIN: &str = "zerotier";
const POLL: Duration = Duration::from_secs(60);

/// Logging to TheHome's log through stderr.
#[doc(hidden)]
pub fn emit(level: &str, message: String) {
    thehome_plugin::log(level, message);
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { $crate::emit("INFO", format!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { $crate::emit("WARN", format!($($t)*)) };
}

fn token_schema() -> Value {
    json!({
        "type": "object",
        "required": ["api_token"],
        "properties": {
            "api_token": {
                "type": "string", "format": "password", "title": "API-token",
                "description": "Lav et på my.zerotier.com under Account → API Access Tokens"
            }
        }
    })
}

fn networks_schema(networks: &[Network], selected: &[String]) -> Value {
    let options: Vec<Value> = networks.iter().map(|n| json!({ "const": n.id, "title": format!("{} ({})", n.name(), n.id) })).collect();
    json!({
        "type": "object",
        "properties": {
            "networks": {
                "type": "array", "title": "Netværk", "items": { "type": "string", "oneOf": options },
                "default": selected,
                "description": "Med alle valgt kommer nye netværk også med"
            }
        }
    })
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn error_code(e: &ApiError) -> &'static str {
    match e {
        ApiError::Auth => "invalid_auth",
        ApiError::Other(_) => "cannot_connect",
    }
}

fn base_url(get: impl Fn(&str) -> Option<String>) -> String {
    // tests point this at a mock server
    get("api_url").unwrap_or_else(|| api::BASE_URL.to_string())
}

fn str_list(v: &Value) -> Option<Vec<String>> {
    v.as_array().map(|a| a.iter().filter_map(Value::as_str).map(String::from).collect())
}

/// The chosen networks: `None` means all, including ones made later.
fn chosen(input: &Value, networks: &[Network]) -> Result<Option<Vec<String>>, ()> {
    let list: Vec<String> = str_list(&input["networks"]).unwrap_or_default().into_iter().filter(|id| networks.iter().any(|n| &n.id == id)).collect();
    if list.is_empty() {
        Err(())
    } else if networks.iter().all(|n| list.contains(&n.id)) {
        Ok(None)
    } else {
        Ok(Some(list))
    }
}

fn selection(entry: &Entry) -> Option<Vec<String>> {
    str_list(&entry.options["networks"]).or_else(|| entry.get("networks").and_then(str_list))
}

#[derive(Default)]
pub struct ZeroTier {
    accounts: Mutex<HashMap<String, Arc<Account>>>,
}

impl ZeroTier {
    fn account(&self, entry_id: &str) -> Result<Arc<Account>, Error> {
        self.accounts.lock().unwrap().get(entry_id).cloned().ok_or_else(|| Error::unavailable("opsætningen kører ikke"))
    }
}

#[async_trait]
impl Integration for ZeroTier {
    async fn config_flow(&self, _host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        match (step, input) {
            ("user", None) | ("reauth", None) => Ok(FlowReply::form("user", token_schema())),
            ("user", Some(v)) => {
                let token = v["api_token"].as_str().unwrap_or_default().trim().to_string();
                let client = Client::new(&base_url(|k| flow.data.get(k).and_then(Value::as_str).map(String::from)), &token);
                let networks = match client.networks().await {
                    Ok(n) => n,
                    Err(e) => {
                        info!("{e}");
                        return Ok(FlowReply::form_with_errors("user", token_schema(), json!({ "base": error_code(&e) })));
                    }
                };
                if flow.source == "reauth" {
                    let mut data = flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default();
                    data.insert("api_token".into(), json!(token));
                    return Ok(FlowReply::create("", Value::Object(data)));
                }
                if networks.is_empty() {
                    return Ok(FlowReply::abort("no_devices_found"));
                }
                let mut data = flow.data.clone();
                data.insert("api_token".into(), json!(token));
                let user = client.user_id().await.ok().flatten();
                if networks.len() == 1 {
                    let reply = FlowReply::create("ZeroTier", Value::Object(data));
                    return Ok(match user {
                        Some(u) => reply.with_unique_id(u),
                        None => reply,
                    });
                }
                let all: Vec<String> = networks.iter().map(|n| n.id.clone()).collect();
                let reply = FlowReply::form("networks", networks_schema(&networks, &all)).with_data(data);
                Ok(match user {
                    Some(u) => reply.with_unique_id(u),
                    None => reply,
                })
            }
            ("networks", Some(v)) => {
                let token = flow.data.get("api_token").and_then(Value::as_str).unwrap_or_default();
                let client = Client::new(&base_url(|k| flow.data.get(k).and_then(Value::as_str).map(String::from)), token);
                let networks = client.networks().await.map_err(Error::failed)?;
                let mut data = flow.data.clone();
                match chosen(&v, &networks) {
                    Err(()) => {
                        let all: Vec<String> = networks.iter().map(|n| n.id.clone()).collect();
                        Ok(FlowReply::form_with_errors("networks", networks_schema(&networks, &all), json!({ "networks": "required" })))
                    }
                    Ok(list) => {
                        if let Some(list) = list {
                            data.insert("networks".into(), json!(list));
                        }
                        Ok(FlowReply::create("ZeroTier", Value::Object(data)))
                    }
                }
            }
            (s, _) => Err(Error::invalid(format!("ukendt trin {s}"))),
        }
    }

    async fn options_flow(&self, _host: &Host, flow: Flow, _step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let Some(entry) = flow.entry.as_ref() else { return Ok(FlowReply::abort("unknown")) };
        let client = Client::new(&base_url(|k| entry.get_str(k).map(String::from)), entry.get_str("api_token").unwrap_or_default());
        let networks = match client.networks().await {
            Ok(n) => n,
            Err(e) => {
                info!("{e}");
                return Ok(FlowReply::abort(if matches!(e, ApiError::Auth) { "invalid_auth" } else { "cannot_connect" }));
            }
        };
        let current = selection(entry).unwrap_or_else(|| networks.iter().map(|n| n.id.clone()).collect());
        match input {
            None => Ok(FlowReply::form("networks", networks_schema(&networks, &current))),
            Some(v) => match chosen(&v, &networks) {
                Err(()) => Ok(FlowReply::form_with_errors("networks", networks_schema(&networks, &current), json!({ "networks": "required" }))),
                Ok(list) => Ok(FlowReply::create("", list.map(|l| json!({ "networks": l })).unwrap_or_else(|| json!({})))),
            },
        }
    }

    async fn setup_entry(&self, host: &Host, entry: Entry) -> Result<(), Error> {
        let entry_id = entry.entry_id.clone();
        let token = entry.get_str("api_token").filter(|t| !t.is_empty()).ok_or_else(|| Error::auth_failed("API-token mangler"))?;
        let client = Client::new(&base_url(|k| entry.get_str(k).map(String::from)), token);
        let networks = match client.networks().await {
            Ok(n) => n,
            Err(ApiError::Auth) => return Err(Error::auth_failed("ZeroTier afviste API-tokenet")),
            Err(e) => return Err(Error::not_ready(e)),
        };
        host.delete_issue(&entry_id, "auth").await?;

        let account = Arc::new(Account {
            entry_id: entry_id.clone(),
            host: host.clone(),
            client,
            selected: selection(&entry),
            state: Mutex::new(State::default()),
            auth_issue: AtomicBool::new(false),
            task: Mutex::new(None),
        });
        account.refresh(Ok(networks)).await;

        // tests poll faster
        let every = entry.get_f64("poll_seconds").filter(|s| *s >= 0.1).map(Duration::from_secs_f64).unwrap_or(POLL);
        let acc = account.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                acc.poll().await;
            }
        });
        *account.task.lock().unwrap() = Some(task);
        if let Some(old) = self.accounts.lock().unwrap().insert(entry_id, account) {
            old.stop();
        }
        Ok(())
    }

    async fn unload_entry(&self, _host: &Host, entry_id: &str) -> Result<(), Error> {
        if let Some(a) = self.accounts.lock().unwrap().remove(entry_id) {
            a.stop();
        }
        Ok(())
    }

    async fn entity_service(&self, _host: &Host, entry_id: &str, entity_id: &str, service: &str, _data: Map<String, Value>) -> Result<(), Error> {
        self.account(entry_id)?.call_service(entity_id, service).await
    }

    async fn diagnostics(&self, _host: &Host, entry: Entry) -> Value {
        let mut d = json!({ "data": entry.data, "options": entry.options });
        if d["data"].get("api_token").is_some() {
            d["data"]["api_token"] = json!("**REDACTED**");
        }
        if let Ok(a) = self.account(&entry.entry_id) {
            let s = a.state.lock().unwrap();
            d["networks"] = s
                .networks
                .values()
                .map(|n| json!({ "id": n.network.id, "name": n.network.name(), "available": n.available, "members": n.members.len() }))
                .collect();
        }
        d
    }

    async fn remove_device(&self, _host: &Host, _entry: Entry, _device: Value) -> Result<(), Error> {
        Ok(())
    }
}

/// A state and its attributes.
type StateUpdate = (String, Map<String, Value>);

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    OnlineMembers,
    AuthorizedMembers,
    Online,
    Authorized,
    LastSeen,
    Ip,
    Version,
}

#[derive(Clone, Debug, PartialEq)]
enum Target {
    Network(String),
    /// network id, node id
    Member(String, String),
}

struct MemberState {
    member: Member,
    /// Still in the network's member list.
    present: bool,
}

struct NetState {
    network: Network,
    members: HashMap<String, MemberState>,
    available: bool,
}

#[derive(Default)]
struct State {
    networks: HashMap<String, NetState>,
    /// (entity_id, target, kind)
    entities: Vec<(String, Target, Kind)>,
}

struct Account {
    entry_id: String,
    host: Host,
    client: Client,
    selected: Option<Vec<String>>,
    state: Mutex<State>,
    auth_issue: AtomicBool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Account {
    fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
    }

    fn wanted(&self, id: &str) -> bool {
        self.selected.as_ref().is_none_or(|s| s.iter().any(|x| x == id))
    }

    async fn poll(&self) {
        let networks = self.client.networks().await;
        self.refresh(networks).await;
    }

    /// Bring devices, entities and states up to date with Central.
    async fn refresh(&self, networks: Result<Vec<Network>, ApiError>) {
        let networks = match networks {
            Ok(n) => n,
            Err(e) => {
                self.report(&e).await;
                for n in self.state.lock().unwrap().networks.values_mut() {
                    n.available = false;
                }
                self.publish().await;
                return;
            }
        };
        self.clear_auth_issue().await;

        let listed: Vec<String> = networks.iter().map(|n| n.id.clone()).collect();
        for n in self.state.lock().unwrap().networks.values_mut() {
            if !listed.contains(&n.network.id) {
                n.available = false;
            }
        }

        for network in networks.into_iter().filter(|n| self.wanted(&n.id)) {
            let members = self.client.members(&network.id).await;
            if !self.state.lock().unwrap().networks.contains_key(&network.id) {
                if let Err(e) = self.register_network(&network).await {
                    warn!("kunne ikke oprette netværk {}: {e}", network.id);
                    continue;
                }
            }
            match members {
                Ok(members) => {
                    let members: Vec<Member> = members.into_iter().filter(|m| !m.hidden && !m.node().is_empty()).collect();
                    for m in &members {
                        let known = self.state.lock().unwrap().networks.get(&network.id).is_some_and(|n| n.members.contains_key(m.node()));
                        if !known {
                            if let Err(e) = self.register_member(&network.id, m).await {
                                warn!("kunne ikke oprette medlem {}: {e}", m.node());
                            }
                        }
                    }
                    let mut state = self.state.lock().unwrap();
                    if let Some(n) = state.networks.get_mut(&network.id) {
                        n.network = network;
                        n.available = true;
                        for s in n.members.values_mut() {
                            s.present = false;
                        }
                        for m in members {
                            if let Some(s) = n.members.get_mut(m.node()) {
                                s.member = m;
                                s.present = true;
                            }
                        }
                    }
                }
                Err(e) => {
                    self.report(&e).await;
                    if let Some(n) = self.state.lock().unwrap().networks.get_mut(&network.id) {
                        n.available = false;
                    }
                }
            }
        }
        self.publish().await;
    }

    async fn register_network(&self, network: &Network) -> Result<(), Error> {
        let id = &network.id;
        let device_id = self
            .host
            .register_device(
                &self.entry_id,
                Device {
                    identifiers: vec![(DOMAIN.into(), id.clone())],
                    name: network.name().to_string(),
                    manufacturer: Some("ZeroTier".into()),
                    model: Some("Netværk".into()),
                    serial_number: Some(id.clone()),
                    configuration_url: Some(format!("https://my.zerotier.com/network/{id}")),
                    ..Default::default()
                },
            )
            .await?;
        self.state.lock().unwrap().networks.insert(
            id.clone(),
            NetState { network: network.clone(), members: HashMap::new(), available: true },
        );
        let list = [
            (Kind::OnlineMembers, Entity::new("sensor", format!("{id}-online_members")).name("Medlemmer online").icon("mdi:lan-connect")),
            (Kind::AuthorizedMembers, Entity::new("sensor", format!("{id}-authorized_members")).name("Godkendte medlemmer").icon("mdi:account-multiple-check")),
        ];
        for (kind, e) in list {
            let e = e.device(&device_id).state_class("measurement");
            if let Some(eid) = self.host.add_entity(&self.entry_id, e, false).await? {
                self.state.lock().unwrap().entities.push((eid, Target::Network(id.clone()), kind));
            }
        }
        Ok(())
    }

    async fn register_member(&self, network_id: &str, m: &Member) -> Result<(), Error> {
        let node = m.node().to_string();
        let device_id = self
            .host
            .register_device(
                &self.entry_id,
                Device {
                    identifiers: vec![(DOMAIN.into(), format!("{network_id}-{node}"))],
                    name: m.display_name(),
                    manufacturer: Some("ZeroTier".into()),
                    model: Some("Medlem".into()),
                    sw_version: m.version(),
                    serial_number: Some(node.clone()),
                    configuration_url: Some(format!("https://my.zerotier.com/network/{network_id}")),
                    via_device: Some((DOMAIN.into(), network_id.to_string())),
                    ..Default::default()
                },
            )
            .await?;
        if let Some(n) = self.state.lock().unwrap().networks.get_mut(network_id) {
            n.members.insert(node.clone(), MemberState { member: m.clone(), present: true });
        }
        let uid = |key: &str| format!("{network_id}-{node}-{key}");
        let list = [
            (Kind::Online, Entity::new("binary_sensor", uid("online")).name("Online").device_class("connectivity"), false),
            (Kind::Authorized, Entity::new("switch", uid("authorized")).name("Godkendt").icon("mdi:shield-check").category("config"), true),
            (Kind::LastSeen, Entity::new("sensor", uid("last_seen")).name("Sidst set").device_class("timestamp"), false),
            (Kind::Ip, Entity::new("sensor", uid("ip")).name("ZeroTier-IP").icon("mdi:ip-network"), false),
            (
                Kind::Version,
                Entity::new("sensor", uid("version")).name("Klientversion").icon("mdi:information-outline").category("diagnostic").disabled_by_default(),
                false,
            ),
        ];
        for (kind, e, handler) in list {
            if let Some(eid) = self.host.add_entity(&self.entry_id, e.device(&device_id), handler).await? {
                self.state.lock().unwrap().entities.push((eid, Target::Member(network_id.to_string(), node.clone()), kind));
            }
        }
        Ok(())
    }

    /// Push states for every entity.
    async fn publish(&self) {
        let now = now_ms();
        let updates: Vec<(String, Option<StateUpdate>)> = {
            let state = self.state.lock().unwrap();
            state.entities.iter().map(|(id, target, kind)| (id.clone(), value(&state, target, *kind, now))).collect()
        };
        for (id, v) in updates {
            let res = match v {
                Some((s, attrs)) => self.host.set_state(&id, s, attrs).await,
                None => self.host.set_unavailable(&id).await,
            };
            if let Err(e) = res {
                warn!("kunne ikke opdatere {id}: {e}");
            }
        }
    }

    async fn clear_auth_issue(&self) {
        if self.auth_issue.swap(false, Ordering::Relaxed) {
            let _ = self.host.delete_issue(&self.entry_id, "auth").await;
        }
    }

    async fn report(&self, e: &ApiError) {
        match e {
            ApiError::Auth => {
                if !self.auth_issue.swap(true, Ordering::Relaxed) {
                    warn!("{e}");
                    let res = self
                        .host
                        .create_issue(
                            &self.entry_id,
                            "auth",
                            "error",
                            "ZeroTier: API-token afvist",
                            "ZeroTier afviste API-tokenet. Lav et nyt på my.zerotier.com, og godkend integrationen igen med det.",
                        )
                        .await;
                    if let Err(e) = res {
                        warn!("kunne ikke oprette problem: {e}");
                    }
                }
            }
            ApiError::Other(m) => warn!("{m}"),
        }
    }

    async fn call_service(&self, entity_id: &str, service: &str) -> Result<(), Error> {
        let (target, kind) = self
            .state
            .lock()
            .unwrap()
            .entities
            .iter()
            .find(|(id, _, _)| id == entity_id)
            .map(|(_, t, k)| (t.clone(), *k))
            .ok_or_else(|| Error::new("not_found", entity_id))?;
        let (Kind::Authorized, Target::Member(net, node)) = (kind, &target) else {
            return Err(Error::not_supported(service));
        };
        let current = self.state.lock().unwrap().networks.get(net).and_then(|n| n.members.get(node)).map(|m| m.member.config.authorized);
        let on = match service {
            "turn_on" => true,
            "turn_off" => false,
            "toggle" => !current.unwrap_or(false),
            _ => return Err(Error::not_supported(service)),
        };
        let member = match self.client.set_authorized(net, node, on).await {
            Ok(m) => m,
            Err(e) => {
                self.report(&e).await;
                return Err(Error::failed(e));
            }
        };
        info!("{} er {} på {net}", member.display_name(), if on { "godkendt" } else { "ikke længere godkendt" });
        if let Some(s) = self.state.lock().unwrap().networks.get_mut(net).and_then(|n| n.members.get_mut(node)) {
            // the reply is the full member; keep the old record if it came back thin
            if member.node() == node {
                s.member = member;
            } else {
                s.member.config.authorized = on;
            }
        }
        self.publish().await;
        Ok(())
    }
}

/// The state of one entity, or `None` for unavailable.
fn value(state: &State, target: &Target, kind: Kind, now: i64) -> Option<StateUpdate> {
    match target {
        Target::Network(id) => {
            let n = state.networks.get(id).filter(|n| n.available)?;
            let present: Vec<Member> = n.members.values().filter(|m| m.present).map(|m| m.member.clone()).collect();
            let (online, authorized) = model::counts(&present, now);
            let mut a = Map::new();
            a.insert("network_id".into(), json!(n.network.id));
            a.insert("network_name".into(), json!(n.network.name()));
            a.insert("private".into(), json!(n.network.config.private));
            a.insert("total_members".into(), json!(present.len()));
            let count = if kind == Kind::OnlineMembers { online } else { authorized };
            Some((count.to_string(), a))
        }
        Target::Member(net, node) => {
            let n = state.networks.get(net).filter(|n| n.available)?;
            let m = &n.members.get(node).filter(|m| m.present)?.member;
            let mut a = Map::new();
            let on_off = |b: bool| (if b { "on" } else { "off" }).to_string();
            let s = match kind {
                Kind::Online => {
                    a.insert("node_id".into(), json!(node));
                    a.insert("network_id".into(), json!(net));
                    if let Some(p) = m.physical_ip() {
                        a.insert("physical_address".into(), json!(p));
                    }
                    on_off(m.is_online(now))
                }
                Kind::Authorized => on_off(m.config.authorized),
                Kind::LastSeen => m.last_contact().and_then(model::iso_time).unwrap_or_else(|| "unknown".into()),
                Kind::Ip => {
                    a.insert("ip_assignments".into(), json!(m.config.ip_assignments));
                    m.ip().map(String::from).unwrap_or_else(|| "unknown".into())
                }
                Kind::Version => m.version().unwrap_or_else(|| "unknown".into()),
                Kind::OnlineMembers | Kind::AuthorizedMembers => return None,
            };
            Some((s, a))
        }
    }
}
