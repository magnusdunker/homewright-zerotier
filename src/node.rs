//! "This machine" mode: the plugin is itself a ZeroTier node (libzt), joins
//! one network per entry, and forwards connections that arrive on its
//! ZeroTier address to HomewrightHQ. The user approves the node's address in
//! ZeroTier Central, as for any other device.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use thehome_plugin::{json, Device, Entity, Error, Host, Map, Value};

use crate::zt::NetInfo;
use crate::{info, warn, DOMAIN};

/// Waiting this long for approval raises a repair issue.
pub const APPROVE_AFTER: Duration = Duration::from_secs(60);
/// The node is in this process, so looking is cheap.
pub const POLL: Duration = Duration::from_secs(5);
/// How long setup waits for the node to reach ZeroTier's roots.
pub const ONLINE_WAIT: Duration = Duration::from_secs(45);

/// Status sensor values: libzt's statuses, plus `left` when not a member
/// and `JOINING` before the network has answered at all.
pub const STATUSES: [&str; 9] = ["JOINING", "REQUESTING_CONFIGURATION", "OK", "ACCESS_DENIED", "NOT_FOUND", "PORT_ERROR", "CLIENT_TOO_OLD", "UNKNOWN", "left"];

/// What a status means, for the UI.
pub fn status_text(status: &str) -> &'static str {
    match status {
        "OK" => "Forbundet",
        "JOINING" | "REQUESTING_CONFIGURATION" => "Venter på svar fra netværkets controller",
        "ACCESS_DENIED" => "Venter på godkendelse i ZeroTier Central",
        "NOT_FOUND" => "Netværket findes ikke",
        "PORT_ERROR" => "ZeroTier kunne ikke oprette netværket",
        "CLIENT_TOO_OLD" => "ZeroTier i integrationen er for gammel til netværket",
        "left" => "Ikke medlem af netværket",
        _ => "Ukendt status",
    }
}

/// A network id is 16 hex digits.
pub fn parse_network_id(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() != 16 {
        return None;
    }
    u64::from_str_radix(s, 16).ok()
}

pub fn network_hex(id: u64) -> String {
    format!("{id:016x}")
}

/// Extra ports as written in the options: "1883, 8080".
pub fn parse_ports(s: &str) -> Result<Vec<u16>, String> {
    let mut out = Vec::new();
    for part in s.split([',', ' ', ';']).map(str::trim).filter(|p| !p.is_empty()) {
        match part.parse::<u16>() {
            Ok(p) if p > 0 => {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            _ => return Err(part.to_string()),
        }
    }
    Ok(out)
}

/// The port HomewrightHQ listens on, as the form's default.
pub fn default_port() -> u16 {
    std::env::var("THEHOME_PORT").ok().and_then(|p| p.trim().parse().ok()).unwrap_or(8123)
}

/// A running forward from a ZeroTier address.
pub trait Forward: Send {
    fn active(&self) -> usize;
    fn listen(&self) -> String;
}

/// ZeroTier for the runtime: libzt in production, a fake in tests.
pub trait Backend: Send + Sync + 'static {
    fn start(&self, dir: &Path) -> Result<(), String>;
    fn online(&self) -> bool;
    fn address(&self) -> Option<String>;
    fn version(&self) -> String;
    fn join(&self, net: u64) -> Result<(), String>;
    fn leave(&self, net: u64) -> Result<(), String>;
    fn network(&self, net: u64) -> Option<NetInfo>;
    fn forward(&self, ip: &str, port: u16, target: SocketAddr) -> Result<Box<dyn Forward>, String>;
}

pub struct Libzt;

impl Forward for crate::proxy::Proxy {
    fn active(&self) -> usize {
        crate::proxy::Proxy::active(self)
    }
    fn listen(&self) -> String {
        self.listen.clone()
    }
}

impl Backend for Libzt {
    fn start(&self, dir: &Path) -> Result<(), String> {
        crate::zt::start(dir)
    }
    fn online(&self) -> bool {
        crate::zt::online()
    }
    fn address(&self) -> Option<String> {
        crate::zt::address()
    }
    fn version(&self) -> String {
        env!("ZT_CORE_VERSION").to_string()
    }
    fn join(&self, net: u64) -> Result<(), String> {
        crate::zt::join(net)
    }
    fn leave(&self, net: u64) -> Result<(), String> {
        crate::zt::leave(net)
    }
    fn network(&self, net: u64) -> Option<NetInfo> {
        crate::zt::network(net)
    }
    fn forward(&self, ip: &str, port: u16, target: SocketAddr) -> Result<Box<dyn Forward>, String> {
        crate::proxy::Proxy::start(ip, port, target).map(|p| Box::new(p) as Box<dyn Forward>)
    }
}

/// Where the node keeps its identity: the same for every entry, so the
/// address stays the same.
pub fn node_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("node")
}

/// Start the node and wait until it is online. Returns its address.
pub async fn bring_up(backend: Arc<dyn Backend>, dir: PathBuf, wait: Duration) -> Result<String, String> {
    let b = backend.clone();
    tokio::task::spawn_blocking(move || b.start(&dir)).await.map_err(|e| e.to_string())??;
    let t = Instant::now();
    while !backend.online() {
        if t.elapsed() >= wait {
            return Err("ZeroTier-noden kom ikke online".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    backend.address().ok_or_else(|| "ZeroTier-noden har ingen adresse".into())
}

/// A state and its attributes, or `None` for unavailable.
type StateUpdate = Option<(String, Map<String, Value>)>;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Connected,
    Status,
    Ip,
    Member,
    Connections,
    NodeAddress,
    NodeOnline,
    NodeVersion,
}

struct Forwarding {
    ip: String,
    port: u16,
    fwd: Box<dyn Forward>,
}

#[derive(Default)]
struct State {
    member: bool,
    net: Option<NetInfo>,
    online: bool,
    waiting_since: Option<Instant>,
    forwards: Vec<Forwarding>,
    /// Ports that could not be forwarded, with the reason.
    forward_errors: Vec<String>,
    entities: Vec<(String, Kind)>,
}

pub struct Settings {
    pub network: u64,
    pub port: u16,
    pub extra_ports: Vec<u16>,
    pub member: bool,
    pub poll: Duration,
    pub approve_after: Duration,
    /// The entry's data, rewritten when the member switch changes.
    pub data: Value,
    pub title: String,
}

pub struct Link {
    entry_id: String,
    host: Host,
    backend: Arc<dyn Backend>,
    address: String,
    s: Settings,
    data: Mutex<Value>,
    state: Mutex<State>,
    approve_issue: AtomicBool,
    titled: AtomicBool,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// The entry title: the network's name once it is known.
pub fn title(network: u64, name: Option<&str>) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => format!("ZeroTier {n} ({})", network_hex(network)),
        None => format!("ZeroTier {}", network_hex(network)),
    }
}

impl Link {
    pub async fn start(host: Host, entry_id: String, backend: Arc<dyn Backend>, address: String, s: Settings) -> Result<Arc<Link>, Error> {
        if s.member {
            backend.join(s.network).map_err(Error::not_ready)?;
        }
        let link = Arc::new(Link {
            entry_id,
            host,
            backend,
            address,
            state: Mutex::new(State { member: s.member, ..Default::default() }),
            data: Mutex::new(s.data.clone()),
            s,
            approve_issue: AtomicBool::new(false),
            titled: AtomicBool::new(false),
            task: Mutex::new(None),
        });
        link.register().await?;
        link.poll().await;
        let l = link.clone();
        let every = link.s.poll;
        *link.task.lock().unwrap() = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                l.poll().await;
            }
        }));
        Ok(link)
    }

    /// Stop polling and forwarding, and leave the network in this process.
    /// Membership in ZeroTier Central is untouched.
    pub fn stop(&self) {
        if let Some(t) = self.task.lock().unwrap().take() {
            t.abort();
        }
        self.state.lock().unwrap().forwards.clear();
        let _ = self.backend.leave(self.s.network);
    }

    pub fn has_entity(&self, entity_id: &str) -> bool {
        self.state.lock().unwrap().entities.iter().any(|(id, _)| id == entity_id)
    }

    pub fn diagnostics(&self) -> Value {
        let s = self.state.lock().unwrap();
        json!({
            "node_address": self.address,
            "network_id": network_hex(self.s.network),
            "member": s.member,
            "online": s.online,
            "network": s.net.as_ref().map(|n| json!({ "status": n.status, "name": n.name, "private": n.private, "ipv4": n.ipv4, "ipv6": n.ipv6 })),
            "forwards": s.forwards.iter().map(|f| json!({ "listen": f.fwd.listen(), "port": f.port, "active": f.fwd.active() })).collect::<Vec<_>>(),
            "forward_errors": s.forward_errors,
        })
    }

    async fn register(&self) -> Result<(), Error> {
        let nwid = network_hex(self.s.network);
        let address = &self.address;
        let node_device = self
            .host
            .register_device(
                &self.entry_id,
                Device {
                    identifiers: vec![(DOMAIN.into(), format!("node-{address}"))],
                    name: format!("ZeroTier-node {address}"),
                    manufacturer: Some("ZeroTier".into()),
                    model: Some("Indbygget node (libzt)".into()),
                    sw_version: Some(self.backend.version()),
                    serial_number: Some(address.clone()),
                    ..Default::default()
                },
            )
            .await?;
        let net_device = self
            .host
            .register_device(
                &self.entry_id,
                Device {
                    identifiers: vec![(DOMAIN.into(), format!("node-net-{nwid}"))],
                    name: format!("ZeroTier {nwid}"),
                    manufacturer: Some("ZeroTier".into()),
                    model: Some("Netværk".into()),
                    serial_number: Some(nwid.clone()),
                    via_device: Some((DOMAIN.into(), format!("node-{address}"))),
                    ..Default::default()
                },
            )
            .await?;
        // unique ids carry the network id, so two networks on one node don't clash
        let uid = |key: &str| format!("node-{nwid}-{key}");
        let list = [
            (Kind::Connected, Entity::new("binary_sensor", uid("connected")).name("Forbundet").device_class("connectivity"), &net_device, false),
            (
                Kind::Status,
                Entity::new("sensor", uid("status")).name("Status").device_class("enum").icon("mdi:lan-pending").capabilities(json!({ "options": STATUSES })),
                &net_device,
                false,
            ),
            (Kind::Ip, Entity::new("sensor", uid("ip")).name("ZeroTier-IP").icon("mdi:ip-network"), &net_device, false),
            (Kind::Member, Entity::new("switch", uid("member")).name("Medlem af netværket").icon("mdi:lan-connect").category("config"), &net_device, true),
            (
                Kind::Connections,
                Entity::new("sensor", uid("connections")).name("Aktive forbindelser").icon("mdi:transit-connection-variant").state_class("measurement"),
                &net_device,
                false,
            ),
            (Kind::NodeAddress, Entity::new("sensor", uid("node_address")).name("Node-adresse").icon("mdi:identifier"), &node_device, false),
            (Kind::NodeOnline, Entity::new("binary_sensor", uid("node_online")).name("Online").device_class("connectivity"), &node_device, false),
            (
                Kind::NodeVersion,
                Entity::new("sensor", uid("node_version")).name("ZeroTier-version").icon("mdi:information-outline").category("diagnostic"),
                &node_device,
                false,
            ),
        ];
        for (kind, e, device, handler) in list {
            if let Some(eid) = self.host.add_entity(&self.entry_id, e.device(device), handler).await? {
                self.state.lock().unwrap().entities.push((eid, kind));
            }
        }
        Ok(())
    }

    pub async fn poll(&self) {
        let net = self.s.network;
        let online = self.backend.online();
        let info = if self.state.lock().unwrap().member { self.backend.network(net) } else { None };
        {
            let mut s = self.state.lock().unwrap();
            s.online = online;
            s.net = info;
        }
        self.update_forwards();
        self.check_waiting().await;
        self.publish().await;
        self.retitle().await;
    }

    /// Put the network's name in the entry title once it is known.
    async fn retitle(&self) {
        if self.titled.load(Ordering::Relaxed) {
            return;
        }
        let name = self.state.lock().unwrap().net.as_ref().map(|n| n.name.clone()).filter(|n| !n.is_empty());
        let Some(name) = name else { return };
        self.titled.store(true, Ordering::Relaxed);
        let t = title(self.s.network, Some(&name));
        if t != self.s.title {
            if let Err(e) = self.host.request("entry/update", json!({ "entry_id": self.entry_id, "title": t })).await {
                warn!("kunne ikke omdøbe opsætningen: {e}");
            }
        }
    }

    /// Listen on the network's addresses once approved, and stop otherwise.
    fn update_forwards(&self) {
        let mut s = self.state.lock().unwrap();
        let ok = s.member && s.net.as_ref().is_some_and(|n| n.status == "OK");
        let ips: Vec<String> = if ok { s.net.as_ref().map(|n| n.ipv4.iter().chain(n.ipv6.iter()).cloned().collect()).unwrap_or_default() } else { vec![] };
        let ports: Vec<u16> = std::iter::once(self.s.port).chain(self.s.extra_ports.iter().copied()).collect();
        // drop forwards whose address went away
        s.forwards.retain(|f| ips.contains(&f.ip));
        let mut errors = Vec::new();
        for ip in &ips {
            for &port in &ports {
                if s.forwards.iter().any(|f| &f.ip == ip && f.port == port) {
                    continue;
                }
                let target = SocketAddr::from(([127, 0, 0, 1], port));
                match self.backend.forward(ip, port, target) {
                    Ok(fwd) => {
                        info!("videresender {} til {target}", fwd.listen());
                        s.forwards.push(Forwarding { ip: ip.clone(), port, fwd });
                    }
                    Err(e) => errors.push(format!("{ip}:{port}: {e}")),
                }
            }
        }
        if errors != s.forward_errors {
            for e in &errors {
                warn!("kunne ikke videresende {e}");
            }
            s.forward_errors = errors;
        }
    }

    /// A repair issue while the node has waited too long for approval,
    /// removed once it is approved or has left.
    async fn check_waiting(&self) {
        let waiting_long = {
            let mut s = self.state.lock().unwrap();
            let waiting = s.member && s.net.as_ref().is_none_or(|n| n.status == "ACCESS_DENIED" || n.status == "REQUESTING_CONFIGURATION");
            if waiting && s.online {
                s.waiting_since.get_or_insert_with(Instant::now).elapsed() >= self.s.approve_after
            } else {
                if !waiting {
                    s.waiting_since = None;
                }
                false
            }
        };
        let approved_or_left = {
            let s = self.state.lock().unwrap();
            !s.member || s.net.as_ref().is_some_and(|n| n.status == "OK")
        };
        if waiting_long {
            if !self.approve_issue.swap(true, Ordering::Relaxed) {
                let nwid = network_hex(self.s.network);
                info!("node {} venter på godkendelse i netværk {nwid}", self.address);
                let res = self
                    .host
                    .create_issue(
                        &self.entry_id,
                        "approve",
                        "warning",
                        &format!("ZeroTier: godkend node {}", self.address),
                        &format!(
                            "HomewrightHQ har meldt sig ind i ZeroTier-netværket {nwid}, men er ikke godkendt endnu. Åbn netværket i ZeroTier Central, find medlemmet {}, og godkend det.",
                            self.address
                        ),
                    )
                    .await;
                if let Err(e) = res {
                    warn!("kunne ikke oprette problem: {e}");
                }
            }
        } else if approved_or_left && self.approve_issue.swap(false, Ordering::Relaxed) {
            let _ = self.host.delete_issue(&self.entry_id, "approve").await;
        }
    }

    async fn publish(&self) {
        let updates: Vec<(String, StateUpdate)> = {
            let s = self.state.lock().unwrap();
            s.entities.iter().map(|(id, kind)| (id.clone(), self.value(&s, *kind))).collect()
        };
        for (id, v) in updates {
            let res = match v {
                Some((state, attrs)) => self.host.set_state(&id, state, attrs).await,
                None => self.host.set_unavailable(&id).await,
            };
            if let Err(e) = res {
                warn!("kunne ikke opdatere {id}: {e}");
            }
        }
    }

    fn status(s: &State) -> String {
        match (&s.net, s.member) {
            (_, false) => "left".into(),
            (None, true) => "JOINING".into(),
            (Some(n), true) => n.status.clone(),
        }
    }

    fn value(&self, s: &State, kind: Kind) -> StateUpdate {
        let on_off = |b: bool| (if b { "on" } else { "off" }).to_string();
        let mut a = Map::new();
        let n = s.net.as_ref().filter(|_| s.member);
        let state = match kind {
            Kind::Connected => on_off(n.is_some_and(|n| n.status == "OK")),
            Kind::Status => {
                let status = Self::status(s);
                a.insert("beskrivelse".into(), json!(status_text(&status)));
                a.insert("network_id".into(), json!(network_hex(self.s.network)));
                a.insert("node_address".into(), json!(self.address));
                if let Some(n) = n {
                    a.insert("network_name".into(), json!(n.name));
                    a.insert("type".into(), json!(if n.private { "private" } else { "public" }));
                    a.insert("mac".into(), json!(n.mac));
                }
                let forwards: Vec<String> = s.forwards.iter().map(|f| format!("{} → 127.0.0.1:{}", f.fwd.listen(), f.port)).collect();
                a.insert("forwarding".into(), json!(forwards));
                if !s.forward_errors.is_empty() {
                    a.insert("forward_errors".into(), json!(s.forward_errors));
                }
                status
            }
            Kind::Ip => {
                let ips: Vec<String> = n.map(|n| n.ipv4.iter().chain(n.ipv6.iter()).cloned().collect()).unwrap_or_default();
                a.insert("ip_assignments".into(), json!(ips));
                ips.first().cloned().unwrap_or_else(|| "unknown".into())
            }
            Kind::Member => on_off(s.member),
            Kind::Connections => s.forwards.iter().map(|f| f.fwd.active()).sum::<usize>().to_string(),
            Kind::NodeAddress => self.address.clone(),
            Kind::NodeOnline => on_off(s.online),
            Kind::NodeVersion => self.backend.version(),
        };
        Some((state, a))
    }

    /// The member switch: join or leave, remembered in the entry.
    pub async fn call_service(&self, entity_id: &str, service: &str) -> Result<(), Error> {
        let kind = self.state.lock().unwrap().entities.iter().find(|(id, _)| id == entity_id).map(|(_, k)| *k);
        if kind != Some(Kind::Member) {
            return Err(Error::not_supported(service));
        }
        let member = self.state.lock().unwrap().member;
        let join = match service {
            "turn_on" => true,
            "turn_off" => false,
            "toggle" => !member,
            _ => return Err(Error::not_supported(service)),
        };
        let nwid = network_hex(self.s.network);
        if join {
            self.backend.join(self.s.network).map_err(Error::failed)?;
        } else {
            self.state.lock().unwrap().forwards.clear();
            self.backend.leave(self.s.network).map_err(Error::failed)?;
        }
        self.state.lock().unwrap().member = join;
        info!("{} netværket {nwid}", if join { "meldte sig ind i" } else { "forlod" });
        let data = {
            let mut d = self.data.lock().unwrap();
            if let Some(o) = d.as_object_mut() {
                o.insert("member".into(), json!(join));
            }
            d.clone()
        };
        if let Err(e) = self.host.update_entry_data(&self.entry_id, data).await {
            warn!("kunne ikke gemme medlemskabet: {e}");
        }
        self.poll().await;
        Ok(())
    }
}
