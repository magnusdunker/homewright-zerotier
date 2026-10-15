//! ZeroTier for TheHome, as an external integration. Two ways to set it up:
//!
//! - This machine (the default): the plugin is itself a ZeroTier node,
//!   built in with libzt. The user enters a network id and approves the
//!   node's address in ZeroTier Central. Connections to the node's ZeroTier
//!   address are forwarded to TheHome, so TheHome can be reached over
//!   ZeroTier with nothing installed on the machine. See [`node`].
//! - ZeroTier Central: with an API token, networks and their members as
//!   devices, with a switch that authorizes each member.

pub mod api;
pub mod model;
pub mod node;
pub mod proxy;
pub mod zt;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use thehome_plugin::{async_trait, json, Device, Entity, Entry, Error, Flow, FlowReply, Host, Integration, Map, Value};

use api::{ApiError, Client, Version};
use model::{Member, Network};
use node::{Backend, Link};

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
                "description": "Et API-token fra en service account (Organization → Service Accounts på central.zerotier.com) eller et ældre token fra Account → API Access Tokens på my.zerotier.com"
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
        ApiError::Auth => "invalid_token",
        ApiError::Other(_) => "cannot_connect",
    }
}

/// Talk to Central with the API stored in `api`, or find the one the token
/// belongs to. Tests put mock URLs in `api_url` and `api_v2_url`.
async fn connect(get: impl Fn(&str) -> Option<String>, token: &str) -> Result<(Client, Vec<Network>), ApiError> {
    let v1 = get("api_url").unwrap_or_else(|| api::V1_URL.to_string());
    let v2 = get("api_v2_url").unwrap_or_else(|| api::V2_URL.to_string());
    match get("api").as_deref().and_then(Version::parse) {
        Some(version) => {
            let client = Client::new(version, if version == Version::V1 { &v1 } else { &v2 }, token);
            let networks = client.networks().await?;
            Ok((client, networks))
        }
        None => Client::detect(&v1, &v2, token).await,
    }
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

/// Menu choices. TheHome shows a choice without a translation with `_` and
/// `-` as spaces.
const MENU_NODE: &str = "Denne_maskine";
const MENU_CENTRAL: &str = "ZeroTier_Central_API-token";

fn node_schema(port: u16, description: Option<&str>) -> Value {
    let mut s = json!({
        "type": "object",
        "required": ["network_id", "port"],
        "properties": {
            "network_id": {
                "type": "string", "title": "Netværks-id", "pattern": "^\\s*[0-9a-fA-F]{16}\\s*$",
                "description": "De 16 tegn, netværket har i ZeroTier Central, f.eks. 8056c2e21c000001"
            },
            "port": {
                "type": "integer", "title": "TheHomes port", "default": port, "minimum": 1, "maximum": 65535,
                "description": "Forbindelser til TheHomes ZeroTier-adresse på denne port sendes videre til TheHome"
            }
        }
    });
    if let Some(d) = description {
        s["description"] = json!(d);
    }
    s
}

fn node_options_schema(port: u16, extra: &[u16]) -> Value {
    let extra: Vec<String> = extra.iter().map(u16::to_string).collect();
    json!({
        "type": "object",
        "required": ["port"],
        "properties": {
            "port": {
                "type": "integer", "title": "TheHomes port", "default": port, "minimum": 1, "maximum": 65535,
                "description": "Forbindelser til TheHomes ZeroTier-adresse på denne port sendes videre til TheHome"
            },
            "extra_ports": {
                "type": "string", "title": "Ekstra porte", "default": extra.join(", "),
                "description": "Andre porte på denne maskine, der også skal kunne nås over ZeroTier, adskilt af komma, f.eks. 1883, 8080"
            }
        }
    })
}

fn approve_schema(address: &str, network: u64, port: u16, net: Option<&zt::NetInfo>) -> Value {
    let nwid = node::network_hex(network);
    let text = match net.filter(|n| n.status == "OK") {
        Some(n) => format!(
            "TheHome er med i netværket {} med ZeroTier-IP {}.\n\nTryk Send for at gøre opsætningen færdig. TheHome kan nås på den IP, port {port}.",
            node::title(network, Some(&n.name)).trim_start_matches("ZeroTier "),
            n.ipv4.clone().or_else(|| n.ipv6.clone()).unwrap_or_else(|| "(ingen endnu)".into())
        ),
        None => format!(
            "TheHome har meldt sig ind i ZeroTier-netværket {nwid}.\n\nGodkend nu noden i ZeroTier Central: åbn netværket, find medlemmet med adressen herunder, og sæt flueben ved godkendt (Auth).\n\nNode-adresse: {address}\n\nTryk Send for at gøre opsætningen færdig, før eller efter du har godkendt. Derefter kan TheHome nås på nodens ZeroTier-IP, port {port}."
        ),
    };
    json!({ "type": "object", "description": text, "properties": {} })
}

/// Integer from a form, which may send it as a number or a string.
fn int_field(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

pub struct ZeroTier {
    accounts: Mutex<HashMap<String, Arc<Account>>>,
    links: Mutex<HashMap<String, Arc<Link>>>,
    backend: Arc<dyn Backend>,
}

impl Default for ZeroTier {
    fn default() -> Self {
        Self::with_backend(Arc::new(node::Libzt))
    }
}

impl ZeroTier {
    /// With another ZeroTier than libzt, for tests.
    pub fn with_backend(backend: Arc<dyn Backend>) -> Self {
        Self { accounts: Mutex::default(), links: Mutex::default(), backend }
    }

    fn account(&self, entry_id: &str) -> Result<Arc<Account>, Error> {
        self.accounts.lock().unwrap().get(entry_id).cloned().ok_or_else(|| Error::unavailable("opsætningen kører ikke"))
    }

    fn link(&self, entry_id: &str) -> Option<Arc<Link>> {
        self.links.lock().unwrap().get(entry_id).cloned()
    }

    /// Start the node and wait for it. Tests shorten the wait with `online_wait`.
    async fn node_up(&self, host: &Host, wait: Option<f64>) -> Result<String, String> {
        let wait = wait.filter(|w| *w > 0.0).map(Duration::from_secs_f64).unwrap_or(node::ONLINE_WAIT);
        node::bring_up(self.backend.clone(), node::node_dir(&host.data_dir()), wait).await
    }

    async fn node_step(&self, host: &Host, flow: &Flow, v: Value) -> Result<FlowReply, Error> {
        let port = int_field(&v["port"]).filter(|p| (1..=65535).contains(p)).map(|p| p as u16);
        let schema = |d: Option<&str>| node_schema(port.unwrap_or_else(node::default_port), d);
        let Some(network) = v["network_id"].as_str().and_then(node::parse_network_id) else {
            return Ok(FlowReply::form_with_errors("node", schema(None), json!({ "network_id": "pattern" })));
        };
        let Some(port) = port else {
            return Ok(FlowReply::form_with_errors("node", schema(None), json!({ "port": "invalid_port" })));
        };
        let address = match self.node_up(host, flow.data.get("online_wait").and_then(Value::as_f64)).await {
            Ok(a) => a,
            Err(e) => {
                warn!("{e}");
                let d = "Integrationens ZeroTier-node kom ikke online. Maskinen skal kunne nå internettet over UDP (ZeroTier bruger port 9993 og tilfældige porte). Prøv igen om lidt.";
                return Ok(FlowReply::form_with_errors("node", schema(Some(d)), json!({ "base": "cannot_connect" })));
            }
        };
        self.backend.join(network).map_err(Error::failed)?;
        info!("node {address} meldte sig ind i netværket {}", node::network_hex(network));
        let mut data = flow.data.clone();
        data.insert("mode".into(), json!("node"));
        data.insert("network_id".into(), json!(node::network_hex(network)));
        data.insert("port".into(), json!(port));
        data.insert("member".into(), json!(true));
        data.insert("node_address".into(), json!(address));
        let net = self.backend.network(network);
        Ok(FlowReply::form("approve", approve_schema(&address, network, port, net.as_ref())).with_data(data).with_unique_id(node::network_hex(network)))
    }

    async fn central_step(&self, flow: &Flow, v: Value) -> Result<FlowReply, Error> {
        let token = v["api_token"].as_str().unwrap_or_default().trim().to_string();
        // a new token may belong to the other API, so always detect
        let (client, networks) = match connect(|k| (k != "api").then(|| flow.data.get(k).and_then(Value::as_str).map(String::from)).flatten(), &token).await {
            Ok(c) => c,
            Err(e) => {
                info!("{e}");
                return Ok(FlowReply::form_with_errors("token", token_schema(), json!({ "base": error_code(&e) })));
            }
        };
        if flow.source == "reauth" {
            let mut data = flow.entry.as_ref().and_then(|e| e.data.as_object().cloned()).unwrap_or_default();
            data.insert("api_token".into(), json!(token));
            data.insert("api".into(), json!(client.version().as_str()));
            return Ok(FlowReply::create("", Value::Object(data)));
        }
        if networks.is_empty() {
            return Ok(FlowReply::abort("no_devices_found"));
        }
        let mut data = flow.data.clone();
        data.insert("mode".into(), json!("central"));
        data.insert("api_token".into(), json!(token));
        data.insert("api".into(), json!(client.version().as_str()));
        let user = client.user_id().await.ok().flatten();
        let reply = if networks.len() == 1 {
            FlowReply::create("ZeroTier", Value::Object(data))
        } else {
            let all: Vec<String> = networks.iter().map(|n| n.id.clone()).collect();
            FlowReply::form("networks", networks_schema(&networks, &all)).with_data(data)
        };
        Ok(match user {
            Some(u) => reply.with_unique_id(u),
            None => reply,
        })
    }

    async fn setup_node(&self, host: &Host, entry: &Entry) -> Result<(), Error> {
        let entry_id = entry.entry_id.clone();
        let network = entry.get_str("network_id").and_then(node::parse_network_id).ok_or_else(|| Error::invalid("netværks-id mangler"))?;
        let port = entry.get("port").and_then(int_field).filter(|p| (1..=65535).contains(p)).map(|p| p as u16).unwrap_or_else(node::default_port);
        let extra_ports = match entry.get("extra_ports") {
            Some(Value::Array(a)) => a.iter().filter_map(int_field).filter(|p| (1..=65535).contains(p)).map(|p| p as u16).collect(),
            Some(Value::String(s)) => node::parse_ports(s).unwrap_or_default(),
            _ => vec![],
        };
        let address = self.node_up(host, entry.get_f64("online_wait")).await.map_err(Error::not_ready)?;
        let secs = |k: &str, d: Duration| entry.get_f64(k).filter(|s| *s >= 0.05).map(Duration::from_secs_f64).unwrap_or(d);
        let settings = node::Settings {
            network,
            port,
            extra_ports,
            member: entry.get("member").and_then(Value::as_bool).unwrap_or(true),
            poll: secs("poll_seconds", node::POLL),
            approve_after: secs("approve_after_seconds", node::APPROVE_AFTER),
            data: entry.data.clone(),
            title: entry.title.clone(),
        };
        let link = Link::start(host.clone(), entry_id.clone(), self.backend.clone(), address, settings).await?;
        if let Some(old) = self.links.lock().unwrap().insert(entry_id, link) {
            old.stop();
        }
        Ok(())
    }
}

fn is_node(entry: &Entry) -> bool {
    entry.data.get("mode").and_then(Value::as_str) == Some("node")
}

#[async_trait]
impl Integration for ZeroTier {
    async fn config_flow(&self, host: &Host, flow: Flow, step: &str, input: Option<Value>) -> Result<FlowReply, Error> {
        let reauth_node = flow.source == "reauth" && flow.entry.as_ref().is_some_and(is_node);
        match (step, input) {
            ("user", None) => Ok(FlowReply::menu("user", &[MENU_NODE, MENU_CENTRAL])),
            // a menu choice arrives as {"next_step_id": ..}
            ("user", Some(v)) if v.get("next_step_id").is_some() => match v["next_step_id"].as_str().unwrap_or_default() {
                MENU_NODE | "node" => Ok(FlowReply::form("node", node_schema(node::default_port(), None))),
                MENU_CENTRAL | "token" => Ok(FlowReply::form("token", token_schema())),
                other => Err(Error::invalid(format!("ukendt valg {other}"))),
            },
            (MENU_NODE, None) | ("node", None) => Ok(FlowReply::form("node", node_schema(node::default_port(), None))),
            (MENU_CENTRAL, None) | ("token", None) => Ok(FlowReply::form("token", token_schema())),
            ("node", Some(v)) => self.node_step(host, &flow, v).await,
            ("approve", _) => {
                let network = flow.data.get("network_id").and_then(Value::as_str).and_then(node::parse_network_id).ok_or_else(|| Error::invalid("netværks-id mangler"))?;
                let name = self.backend.network(network).map(|n| n.name);
                Ok(FlowReply::create(&node::title(network, name.as_deref()), Value::Object(flow.data.clone())))
            }
            // the node's identity is local, so there is nothing to sign in again with
            ("reauth", None) if reauth_node => Ok(FlowReply::abort("not_supported")),
            ("reauth", None) => Ok(FlowReply::form("token", token_schema())),
            // "user" with a token is how v0.1 sent it
            ("token", Some(v)) | ("user", Some(v)) => self.central_step(&flow, v).await,
            ("networks", Some(v)) => {
                let token = flow.data.get("api_token").and_then(Value::as_str).unwrap_or_default();
                let (_, networks) = connect(|k| flow.data.get(k).and_then(Value::as_str).map(String::from), token).await.map_err(Error::failed)?;
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
        if is_node(entry) {
            let port = entry.get("port").and_then(int_field).map(|p| p as u16).unwrap_or_else(node::default_port);
            let extra: Vec<u16> = entry.get("extra_ports").and_then(Value::as_array).map(|a| a.iter().filter_map(int_field).map(|p| p as u16).collect()).unwrap_or_default();
            let Some(v) = input else { return Ok(FlowReply::form("node_options", node_options_schema(port, &extra))) };
            let Some(new_port) = int_field(&v["port"]).filter(|p| (1..=65535).contains(p)) else {
                return Ok(FlowReply::form_with_errors("node_options", node_options_schema(port, &extra), json!({ "port": "invalid_port" })));
            };
            return match node::parse_ports(v["extra_ports"].as_str().unwrap_or_default()) {
                Ok(ports) => Ok(FlowReply::create("", json!({ "port": new_port, "extra_ports": ports }))),
                Err(_) => Ok(FlowReply::form_with_errors("node_options", node_options_schema(port, &extra), json!({ "extra_ports": "invalid_port" }))),
            };
        }
        let networks = match connect(|k| entry.get_str(k).map(String::from), entry.get_str("api_token").unwrap_or_default()).await.map(|c| c.1) {
            Ok(n) => n,
            Err(e) => {
                info!("{e}");
                return Ok(FlowReply::abort(if matches!(e, ApiError::Auth) { "invalid_token" } else { "cannot_connect" }));
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
        if is_node(&entry) {
            return self.setup_node(host, &entry).await;
        }
        let entry_id = entry.entry_id.clone();
        let token = entry.get_str("api_token").filter(|t| !t.is_empty()).ok_or_else(|| Error::auth_failed("API-token mangler"))?;
        // entries from v0.1.0 have no `api`; they find it here
        let (client, networks) = match connect(|k| entry.get_str(k).map(String::from), token).await {
            Ok(c) => c,
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
        if let Some(l) = self.links.lock().unwrap().remove(entry_id) {
            l.stop();
        }
        if let Some(a) = self.accounts.lock().unwrap().remove(entry_id) {
            a.stop();
        }
        Ok(())
    }

    async fn entity_service(&self, _host: &Host, entry_id: &str, entity_id: &str, service: &str, _data: Map<String, Value>) -> Result<(), Error> {
        if let Some(l) = self.link(entry_id) {
            return l.call_service(entity_id, service).await;
        }
        self.account(entry_id)?.call_service(entity_id, service).await
    }

    async fn diagnostics(&self, _host: &Host, entry: Entry) -> Value {
        let mut d = json!({ "data": entry.data, "options": entry.options });
        if d["data"].get("api_token").is_some() {
            d["data"]["api_token"] = json!("**REDACTED**");
        }
        if let Some(l) = self.link(&entry.entry_id) {
            d["node"] = l.diagnostics();
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
