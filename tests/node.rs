//! "This machine" mode, driven over a pipe the way TheHome drives it, with a
//! fake ZeroTier in place of libzt so it runs without the internet.

mod host;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use thehome_zerotier::node::{self, Backend, Forward};
use thehome_zerotier::zt::NetInfo;

const NET: &str = "8056c2e21c00abcd";
const NET_ID: u64 = 0x8056c2e21c00abcd;
const ADDR: &str = "a1b2c3d4e5";

#[derive(Default)]
struct FakeState {
    started: Option<PathBuf>,
    online: bool,
    joined: Vec<u64>,
    nets: HashMap<u64, NetInfo>,
    /// listening forwards: (ip, port, target)
    forwards: Vec<(String, u16, SocketAddr)>,
    refuse_port: Option<u16>,
}

#[derive(Default, Clone)]
struct Fake(Arc<Mutex<FakeState>>);

struct FakeForward {
    fake: Fake,
    key: (String, u16),
    active: Arc<AtomicUsize>,
}

impl Forward for FakeForward {
    fn active(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }
    fn listen(&self) -> String {
        format!("{}:{}", self.key.0, self.key.1)
    }
}

impl Drop for FakeForward {
    fn drop(&mut self) {
        self.fake.0.lock().unwrap().forwards.retain(|f| (f.0.clone(), f.1) != self.key);
    }
}

impl Backend for Fake {
    fn start(&self, dir: &Path) -> Result<(), String> {
        self.0.lock().unwrap().started = Some(dir.to_path_buf());
        Ok(())
    }
    fn online(&self) -> bool {
        self.0.lock().unwrap().online
    }
    fn address(&self) -> Option<String> {
        Some(ADDR.into())
    }
    fn version(&self) -> String {
        "1.12.0".into()
    }
    fn join(&self, net: u64) -> Result<(), String> {
        let mut s = self.0.lock().unwrap();
        if !s.joined.contains(&net) {
            s.joined.push(net);
        }
        Ok(())
    }
    fn leave(&self, net: u64) -> Result<(), String> {
        self.0.lock().unwrap().joined.retain(|n| *n != net);
        Ok(())
    }
    fn network(&self, net: u64) -> Option<NetInfo> {
        let s = self.0.lock().unwrap();
        s.joined.contains(&net).then(|| s.nets.get(&net).cloned()).flatten()
    }
    fn forward(&self, ip: &str, port: u16, target: SocketAddr) -> Result<Box<dyn Forward>, String> {
        let mut s = self.0.lock().unwrap();
        if s.refuse_port == Some(port) {
            return Err("address in use".into());
        }
        s.forwards.push((ip.to_string(), port, target));
        Ok(Box::new(FakeForward { fake: self.clone(), key: (ip.to_string(), port), active: Arc::new(AtomicUsize::new(0)) }))
    }
}

impl Fake {
    fn set(&self, status: &str, name: &str, ipv4: Option<&str>) {
        let info = NetInfo {
            status: status.into(),
            name: name.into(),
            private: true,
            mac: "1a:31:9e:bc:d7:8c".into(),
            ipv4: ipv4.map(String::from),
            ipv6: None,
        };
        self.0.lock().unwrap().nets.insert(NET_ID, info);
    }
    fn forwards(&self) -> Vec<String> {
        let mut v: Vec<String> = self.0.lock().unwrap().forwards.iter().map(|f| format!("{}:{}->{}", f.0, f.1, f.2)).collect();
        v.sort();
        v
    }
    fn joined(&self) -> bool {
        self.0.lock().unwrap().joined.contains(&NET_ID)
    }
}

fn flow(source: &str, data: Value) -> Value {
    json!({ "flow_id": "f1", "source": source, "data": data })
}

#[test]
fn parsing() {
    assert_eq!(node::parse_network_id(" 8056C2E21C00ABCD "), Some(NET_ID));
    assert_eq!(node::parse_network_id("8056c2e21c00abc"), None);
    assert_eq!(node::parse_network_id("8056c2e21c00abcx"), None);
    assert_eq!(node::network_hex(0x00ff00000000000a), "00ff00000000000a");
    assert_eq!(node::parse_ports("1883, 8080;8080 9000"), Ok(vec![1883, 8080, 9000]));
    assert_eq!(node::parse_ports(""), Ok(vec![]));
    assert_eq!(node::parse_ports("80, x"), Err("x".into()));
    assert_eq!(node::parse_ports("0"), Err("0".into()));
    assert_eq!(node::title(NET_ID, None), format!("ZeroTier {NET}"));
    assert_eq!(node::title(NET_ID, Some(" Hjem ")), format!("ZeroTier Hjem ({NET})"));
    assert_eq!(thehome_zerotier::zt::status_name(2), "ACCESS_DENIED");
    assert_eq!(thehome_zerotier::zt::status_name(9), "UNKNOWN");
    assert_eq!(node::status_text("ACCESS_DENIED"), "Venter på godkendelse i ZeroTier Central");
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_flow() {
    let fake = Fake::default();
    let h = host::FakeHost::start(thehome_zerotier::ZeroTier::with_backend(Arc::new(fake.clone())));
    let start = json!({ "online_wait": 0.3 });

    // this machine is the first choice
    let r = h.call("config_flow", json!({ "flow": flow("user", start.clone()), "step": "user", "input": { "next_step_id": "Denne_maskine" } })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "node");
    let props = r["result"]["schema"]["properties"].as_object().unwrap();
    assert_eq!(props.keys().collect::<Vec<_>>(), ["network_id", "port"], "the network id comes first");
    assert_eq!(props["port"]["default"], node::default_port());

    let step = |input: Value| json!({ "flow": flow("user", start.clone()), "step": "node", "input": input });
    let r = h.call("config_flow", step(json!({ "network_id": "nope", "port": 8124 }))).await.unwrap();
    assert_eq!(r["result"]["errors"]["network_id"], "pattern");
    let r = h.call("config_flow", step(json!({ "network_id": NET, "port": 0 }))).await.unwrap();
    assert_eq!(r["result"]["errors"]["port"], "invalid_port");

    // the node doesn't reach ZeroTier
    let r = h.call("config_flow", step(json!({ "network_id": NET, "port": 8124 }))).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "cannot_connect");
    assert!(r["result"]["schema"]["description"].as_str().unwrap().contains("UDP"));
    assert!(!fake.joined());

    // online: joined, and the address to approve is shown
    fake.0.lock().unwrap().online = true;
    let r = h.call("config_flow", step(json!({ "network_id": format!(" {} ", NET.to_uppercase()), "port": "8124" }))).await.unwrap();
    assert!(fake.joined());
    assert!(fake.0.lock().unwrap().started.as_ref().unwrap().ends_with("node"));
    assert_eq!(r["result"]["step_id"], "approve");
    assert_eq!(r["unique_id"], NET);
    let text = r["result"]["schema"]["description"].as_str().unwrap();
    assert!(text.contains(&format!("Node-adresse: {ADDR}")), "{text}");
    assert!(text.contains("port 8124"));
    assert_eq!(r["data"]["mode"], "node");
    assert_eq!(r["data"]["network_id"], NET);
    assert_eq!(r["data"]["port"], 8124);
    assert_eq!(r["data"]["member"], true);
    let data = r["data"].clone();

    // finish before approval: titled with the id
    let r = h.call("config_flow", json!({ "flow": flow("user", data.clone()), "step": "approve", "input": {} })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["title"], format!("ZeroTier {NET}"));
    // once approved the name is known
    fake.set("OK", "Hjem", Some("10.147.17.5"));
    let r = h.call("config_flow", json!({ "flow": flow("user", data.clone()), "step": "approve", "input": {} })).await.unwrap();
    assert_eq!(r["result"]["title"], format!("ZeroTier Hjem ({NET})"));

    // a node entry has no token to renew
    let mut reauth = flow("reauth", json!({}));
    reauth["entry"] = json!({ "entry_id": "e1", "data": data });
    let r = h.call("config_flow", json!({ "flow": reauth, "step": "reauth", "input": null })).await.unwrap();
    assert_eq!(r["result"]["type"], "abort");

    // options: port and extra ports
    let mut opts = flow("options", json!({}));
    opts["entry"] = json!({ "entry_id": "e1", "data": data, "options": { "extra_ports": [1883] } });
    let r = h.call("options_flow", json!({ "flow": opts, "step": "init", "input": null })).await.unwrap();
    assert_eq!(r["result"]["schema"]["properties"]["port"]["default"], 8124);
    assert_eq!(r["result"]["schema"]["properties"]["extra_ports"]["default"], "1883");
    let r = h.call("options_flow", json!({ "flow": opts, "step": "node_options", "input": { "port": 8124, "extra_ports": "1883, x" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["extra_ports"], "invalid_port");
    let r = h.call("options_flow", json!({ "flow": opts, "step": "node_options", "input": { "port": 8125, "extra_ports": "1883, 8080" } })).await.unwrap();
    assert_eq!(r["result"]["data"], json!({ "port": 8125, "extra_ports": [1883, 8080] }));
}

async fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{what}");
}

#[tokio::test(flavor = "multi_thread")]
async fn node_runtime() {
    let fake = Fake::default();
    fake.0.lock().unwrap().online = true;
    let h = host::FakeHost::start(thehome_zerotier::ZeroTier::with_backend(Arc::new(fake.clone())));
    let data = json!({ "mode": "node", "network_id": NET, "port": 8124, "member": true, "node_address": ADDR,
                       "poll_seconds": 0.1, "approve_after_seconds": 0.3, "online_wait": 0.3 });
    let entry = json!({ "entry_id": "e1", "domain": "zerotier", "title": format!("ZeroTier {NET}"), "data": data, "options": { "extra_ports": [1883] } });
    h.call("setup_entry", json!({ "entry": entry.clone() })).await.unwrap();
    assert!(fake.joined());

    let uid = |k: &str| format!("node-{NET}-{k}");
    {
        let rec = h.rec.lock().unwrap();
        assert_eq!(rec.devices[0]["identifiers"], json!([["zerotier", format!("node-{ADDR}")]]));
        assert_eq!(rec.devices[0]["name"], format!("ZeroTier-node {ADDR}"));
        assert_eq!(rec.devices[0]["sw_version"], "1.12.0");
        assert_eq!(rec.devices[1]["via_device"], json!(["zerotier", format!("node-{ADDR}")]));
    }
    assert_eq!(h.state_of(&uid("node_address")).unwrap().0, ADDR);
    assert_eq!(h.state_of(&uid("node_online")).unwrap().0, "on");
    assert_eq!(h.state_of(&uid("member")).unwrap().0, "on");
    // nothing from the network yet
    assert_eq!(h.state_of(&uid("status")).unwrap().0, "JOINING");
    assert_eq!(h.state_of(&uid("connected")).unwrap().0, "off");
    assert_eq!(h.state_of(&uid("ip")).unwrap().0, "unknown");
    assert!(fake.forwards().is_empty());

    // waiting for approval: a repair issue after a while
    fake.set("ACCESS_DENIED", "", None);
    wait_until("status", || h.state_of(&uid("status")).is_some_and(|s| s.0 == "ACCESS_DENIED")).await;
    let (_, a) = h.state_of(&uid("status")).unwrap();
    assert_eq!(a["beskrivelse"], "Venter på godkendelse i ZeroTier Central");
    assert_eq!(a["node_address"], ADDR);
    wait_until("approve issue", || h.rec.lock().unwrap().issues.contains(&"approve".to_string())).await;

    // approved: connected, forwarding TheHome's port and the extra one, issue gone, title with the name
    fake.set("OK", "Hjem", Some("10.147.17.5"));
    wait_until("connected", || h.state_of(&uid("connected")).is_some_and(|s| s.0 == "on")).await;
    wait_until("forwards", || fake.forwards().len() == 2).await;
    assert_eq!(fake.forwards(), ["10.147.17.5:1883->127.0.0.1:1883", "10.147.17.5:8124->127.0.0.1:8124"]);
    wait_until("issue removed", || h.rec.lock().unwrap().issues.is_empty()).await;
    wait_until("ip", || h.state_of(&uid("ip")).is_some_and(|s| s.0 == "10.147.17.5")).await;
    let (_, a) = h.state_of(&uid("status")).unwrap();
    assert_eq!(a["network_name"], "Hjem");
    assert_eq!(a["type"], "private");
    assert_eq!(a["forwarding"].as_array().unwrap().len(), 2);
    assert_eq!(h.state_of(&uid("connections")).unwrap().0, "0");
    wait_until("title", || !h.rec.lock().unwrap().updates.is_empty()).await;
    assert_eq!(h.rec.lock().unwrap().updates[0], json!({ "entry_id": "e1", "title": format!("ZeroTier Hjem ({NET})") }));

    // the switch leaves: forwards stop, and the choice is kept in the entry
    let call = |service: &str| json!({ "entry_id": "e1", "entity_id": h.entity(&uid("member")), "service": service, "data": {} });
    h.call("entity/call_service", call("turn_off")).await.unwrap();
    assert!(!fake.joined());
    assert!(fake.forwards().is_empty());
    assert_eq!(h.state_of(&uid("member")).unwrap().0, "off");
    assert_eq!(h.state_of(&uid("status")).unwrap().0, "left");
    assert_eq!(h.state_of(&uid("connected")).unwrap().0, "off");
    let last = h.rec.lock().unwrap().updates.last().unwrap().clone();
    assert_eq!(last["data"]["member"], false);
    assert_eq!(last["data"]["network_id"], NET, "the rest of the data is kept");
    h.call("entity/call_service", call("toggle")).await.unwrap();
    assert!(fake.joined());
    wait_until("forwards again", || fake.forwards().len() == 2).await;
    let e = h.call("entity/call_service", json!({ "entry_id": "e1", "entity_id": h.entity(&uid("connected")), "service": "turn_off", "data": {} })).await.unwrap_err();
    assert_eq!(e["data"]["kind"], "not_supported");

    let diag = h.call("diagnostics", json!({ "entry": entry })).await.unwrap();
    assert_eq!(diag["node"]["node_address"], ADDR);
    assert_eq!(diag["node"]["network"]["status"], "OK");

    // unload stops forwarding and leaves locally
    h.call("unload_entry", json!({ "entry_id": "e1" })).await.unwrap();
    assert!(fake.forwards().is_empty());
    assert!(!fake.joined());
}

#[tokio::test(flavor = "multi_thread")]
async fn left_entry_stays_out_and_port_errors_show() {
    let fake = Fake::default();
    fake.0.lock().unwrap().online = true;
    fake.0.lock().unwrap().refuse_port = Some(1883);
    fake.set("OK", "Hjem", Some("10.147.17.5"));
    let h = host::FakeHost::start(thehome_zerotier::ZeroTier::with_backend(Arc::new(fake.clone())));
    let data = json!({ "mode": "node", "network_id": NET, "port": 8124, "member": false, "poll_seconds": 0.1, "online_wait": 0.3 });
    h.call("setup_entry", json!({ "entry": { "entry_id": "e2", "domain": "zerotier", "data": data, "options": {} } })).await.unwrap();
    let uid = |k: &str| format!("node-{NET}-{k}");
    assert!(!fake.joined(), "a node that left stays out after a restart");
    assert_eq!(h.state_of(&uid("status")).unwrap().0, "left");

    h.call("entity/call_service", json!({ "entry_id": "e2", "entity_id": h.entity(&uid("member")), "service": "turn_on", "data": {} })).await.unwrap();
    h.call("unload_entry", json!({ "entry_id": "e2" })).await.unwrap();

    // a port that can't be forwarded is reported, the others still work
    let data = json!({ "mode": "node", "network_id": NET, "port": 8124, "extra_ports": [1883], "poll_seconds": 0.1, "online_wait": 0.3 });
    h.call("setup_entry", json!({ "entry": { "entry_id": "e3", "domain": "zerotier", "data": data, "options": {} } })).await.unwrap();
    assert_eq!(fake.forwards(), ["10.147.17.5:8124->127.0.0.1:8124"]);
    let (_, a) = h.state_of(&uid("status")).unwrap();
    assert_eq!(a["forward_errors"], json!(["10.147.17.5:1883: address in use"]));

    // offline at startup: not ready, so TheHome retries
    let fake2 = Fake::default();
    let h2 = host::FakeHost::start(thehome_zerotier::ZeroTier::with_backend(Arc::new(fake2)));
    let data = json!({ "mode": "node", "network_id": NET, "port": 8124, "online_wait": 0.2 });
    let e = h2.call("setup_entry", json!({ "entry": { "entry_id": "e4", "domain": "zerotier", "data": data, "options": {} } })).await.unwrap_err();
    assert_eq!(e["data"]["kind"], "not_ready");
}
