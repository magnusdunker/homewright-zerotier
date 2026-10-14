//! Model and API tests against a fake Central, and the plugin driven over a
//! pipe the way TheHome drives it.

mod host;

use std::time::Duration;

use serde_json::{json, Value};
use thehome_zerotier::api::{ApiError, Client};
use thehome_zerotier::model::{self, Member, Network};
use wiremock::matchers::{any, body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const NET: &str = "8056c2e21c000001";
const NET2: &str = "8056c2e21c000002";
const TOKEN: &str = "good-token";

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn network(id: &str, name: &str) -> Value {
    json!({ "id": id, "clock": 1, "config": { "id": id, "name": name, "private": true }, "description": "",
            "onlineMemberCount": 0, "authorizedMemberCount": 0, "totalMemberCount": 0 })
}

fn member(net: &str, node: &str, name: &str, authorized: bool, seen_ago_ms: i64) -> Value {
    json!({
        "id": format!("{net}-{node}"), "networkId": net, "nodeId": node, "name": name, "description": "", "hidden": false,
        "config": { "id": node, "authorized": authorized, "ipAssignments": ["10.147.17.5", "fd80::5"],
                    "vMajor": 1, "vMinor": 14, "vRev": 2, "creationTime": 1_700_000_000_000i64 },
        "lastOnline": now() - seen_ago_ms, "lastSeen": now() - seen_ago_ms,
        "physicalAddress": "203.0.113.7", "clientVersion": "1.14.2", "protocolVersion": 13
    })
}

#[test]
fn member_values() {
    let m: Member = serde_json::from_value(member(NET, "a1b2c3d4e5", "Laptop", true, 60_000)).unwrap();
    assert_eq!(m.node(), "a1b2c3d4e5");
    assert_eq!(m.display_name(), "Laptop");
    assert!(m.is_online(now()));
    assert!(!m.is_online(now() + model::ONLINE_WINDOW_MS));
    assert_eq!(m.version().as_deref(), Some("1.14.2"));
    assert_eq!(m.ip(), Some("10.147.17.5"));
    assert_eq!(m.physical_ip(), Some("203.0.113.7"));

    // Central leaves fields out or zeroes them
    let bare: Member = serde_json::from_value(json!({ "config": { "id": "ffeeddccbb", "vMajor": -1, "vMinor": -1, "vRev": -1 }, "lastSeen": 0 })).unwrap();
    assert_eq!(bare.node(), "ffeeddccbb");
    assert_eq!(bare.display_name(), "ffeeddccbb");
    assert_eq!(bare.last_contact(), None);
    assert!(!bare.is_online(now()));
    assert_eq!(bare.version(), None);
    assert_eq!(bare.ip(), None);

    let old: Member = serde_json::from_value(json!({ "nodeId": "x", "clientVersion": "", "config": { "vMajor": 1, "vMinor": 12, "vRev": 0 }, "lastOnline": 5, "lastSeen": 9 })).unwrap();
    assert_eq!(old.version().as_deref(), Some("1.12.0"));
    assert_eq!(old.last_contact(), Some(9));

    assert_eq!(model::iso_time(1_700_000_000_000).as_deref(), Some("2023-11-14T22:13:20+00:00"));

    let n: Network = serde_json::from_value(network(NET, "")).unwrap();
    assert_eq!(n.name(), NET);
    let n: Network = serde_json::from_value(network(NET, "Hjem")).unwrap();
    assert_eq!(n.name(), "Hjem");
}

#[test]
fn counts_only_authorized_online() {
    let ms: Vec<Member> = [
        member(NET, "1111111111", "a", true, 1000),
        member(NET, "2222222222", "b", true, 3_600_000),
        member(NET, "3333333333", "c", false, 1000),
    ]
    .into_iter()
    .map(|v| serde_json::from_value(v).unwrap())
    .collect();
    assert_eq!(model::counts(&ms, now()), (1, 2));
}

/// Central with two networks; the first has an online laptop and an offline,
/// unauthorized phone. Anything without the right token gets 401.
async fn mock_central(server: &MockServer, members: Vec<Value>) {
    Mock::given(any()).respond_with(ResponseTemplate::new(401)).with_priority(10).mount(server).await;
    let auth = || header("Authorization", format!("token {TOKEN}").as_str());
    Mock::given(method("GET")).and(path("/network")).and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([network(NET, "Hjem"), network(NET2, "Arbejde")])))
        .mount(server).await;
    Mock::given(method("GET")).and(path("/status")).and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "s", "user": { "id": "user-1" } })))
        .mount(server).await;
    Mock::given(method("GET")).and(path(format!("/network/{NET}/member"))).and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(members)))
        .mount(server).await;
    Mock::given(method("GET")).and(path(format!("/network/{NET2}/member"))).and(auth())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .mount(server).await;
    Mock::given(method("POST")).and(path(format!("/network/{NET}/member/a1b2c3d4e5"))).and(auth())
        .respond_with(|r: &Request| {
            let body: Value = serde_json::from_slice(&r.body).unwrap();
            let mut m = member(NET, "a1b2c3d4e5", "Laptop", true, 60_000);
            m["config"]["authorized"] = body["config"]["authorized"].clone();
            ResponseTemplate::new(200).set_body_json(m)
        })
        .mount(server).await;
}

fn first_members() -> Vec<Value> {
    vec![member(NET, "a1b2c3d4e5", "Laptop", true, 60_000), member(NET, "0f0f0f0f0f", "", false, 86_400_000)]
}

#[tokio::test]
async fn api_client() {
    let server = MockServer::start().await;
    mock_central(&server, first_members()).await;
    let c = Client::new(&server.uri(), TOKEN);
    let nets = c.networks().await.unwrap();
    assert_eq!(nets.len(), 2);
    assert_eq!(nets[1].name(), "Arbejde");
    assert_eq!(c.user_id().await.unwrap().as_deref(), Some("user-1"));
    let ms = c.members(NET).await.unwrap();
    assert_eq!(ms.len(), 2);
    assert!(ms[0].config.authorized);
    let m = c.set_authorized(NET, "a1b2c3d4e5", false).await.unwrap();
    assert!(!m.config.authorized);
    let posted = server.received_requests().await.unwrap().into_iter().find(|r| r.method.as_str() == "POST").unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&posted.body).unwrap(), json!({ "config": { "authorized": false } }));

    let bad = Client::new(&server.uri(), "nope");
    assert!(matches!(bad.networks().await, Err(ApiError::Auth)));
    let down = Client::new("http://127.0.0.1:9", TOKEN);
    assert!(matches!(down.networks().await, Err(ApiError::Other(_))));
}

#[tokio::test]
async fn post_body_is_only_authorized() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(body_json(json!({ "config": { "authorized": true } })))
        .respond_with(ResponseTemplate::new(200).set_body_json(member(NET, "a1b2c3d4e5", "Laptop", true, 0)))
        .expect(1)
        .mount(&server).await;
    Client::new(&server.uri(), TOKEN).set_authorized(NET, "a1b2c3d4e5", true).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_flow() {
    let server = MockServer::start().await;
    mock_central(&server, first_members()).await;
    let h = host::FakeHost::start(thehome_zerotier::ZeroTier::default());
    let flow = |source: &str, data: Value| json!({ "flow_id": "f1", "source": source, "data": data });
    let start = json!({ "api_url": server.uri() });

    let r = h.call("config_flow", json!({ "flow": flow("user", start.clone()), "step": "user", "input": null })).await.unwrap();
    assert_eq!(r["result"]["type"], "form");
    let keys: Vec<&String> = r["result"]["schema"]["properties"].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["api_token"]);

    let r = h.call("config_flow", json!({ "flow": flow("user", start.clone()), "step": "user", "input": { "api_token": "nope" } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["base"], "invalid_auth");
    let r = h
        .call("config_flow", json!({ "flow": flow("user", json!({ "api_url": "http://127.0.0.1:9" })), "step": "user", "input": { "api_token": TOKEN } }))
        .await
        .unwrap();
    assert_eq!(r["result"]["errors"]["base"], "cannot_connect");

    // two networks: pick among them, all checked to begin with
    let r = h.call("config_flow", json!({ "flow": flow("user", start.clone()), "step": "user", "input": { "api_token": format!(" {TOKEN} ") } })).await.unwrap();
    assert_eq!(r["result"]["step_id"], "networks");
    assert_eq!(r["unique_id"], "user-1");
    assert_eq!(r["data"]["api_token"], TOKEN);
    let prop = &r["result"]["schema"]["properties"]["networks"];
    assert_eq!(prop["default"], json!([NET, NET2]));
    assert_eq!(prop["items"]["oneOf"][0], json!({ "const": NET, "title": format!("Hjem ({NET})") }));
    let data = r["data"].clone();

    let r = h.call("config_flow", json!({ "flow": flow("user", data.clone()), "step": "networks", "input": { "networks": [] } })).await.unwrap();
    assert_eq!(r["result"]["errors"]["networks"], "required");
    let r = h.call("config_flow", json!({ "flow": flow("user", data.clone()), "step": "networks", "input": { "networks": [NET, NET2] } })).await.unwrap();
    assert_eq!(r["result"]["type"], "create_entry");
    assert_eq!(r["result"]["title"], "ZeroTier");
    assert!(r["result"]["data"].get("networks").is_none(), "all networks are stored as no choice");
    let r = h.call("config_flow", json!({ "flow": flow("user", data.clone()), "step": "networks", "input": { "networks": [NET2] } })).await.unwrap();
    assert_eq!(r["result"]["data"]["networks"], json!([NET2]));

    // reauth keeps the entry's other data
    let mut reauth = flow("reauth", start.clone());
    reauth["entry"] = json!({ "entry_id": "e1", "data": { "api_token": "old", "networks": [NET] } });
    let r = h.call("config_flow", json!({ "flow": reauth, "step": "user", "input": { "api_token": TOKEN } })).await.unwrap();
    assert_eq!(r["result"]["data"], json!({ "api_token": TOKEN, "networks": [NET] }));

    // options: change the networks
    let mut opts = flow("options", json!({}));
    opts["entry"] = json!({ "entry_id": "e1", "data": { "api_token": TOKEN, "api_url": server.uri() }, "options": { "networks": [NET] } });
    let r = h.call("options_flow", json!({ "flow": opts, "step": "options", "input": null })).await.unwrap();
    assert_eq!(r["result"]["schema"]["properties"]["networks"]["default"], json!([NET]));
    let r = h.call("options_flow", json!({ "flow": opts, "step": "networks", "input": { "networks": [NET, NET2] } })).await.unwrap();
    assert_eq!(r["result"]["data"], json!({}));

    let e = h
        .call("setup_entry", json!({ "entry": { "entry_id": "e1", "domain": "zerotier", "data": { "api_token": "nope", "api_url": server.uri() }, "options": {} } }))
        .await
        .unwrap_err();
    assert_eq!(e["data"]["kind"], "auth_failed");
}

async fn wait_for(h: &host::FakeHost, unique_id: &str, want: &str) {
    for _ in 0..300 {
        if h.state_of(unique_id).is_some_and(|s| s.0 == want) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{unique_id} blev ikke {want}: {:?}", h.state_of(unique_id));
}

#[tokio::test(flavor = "multi_thread")]
async fn plugin_setup_and_control() {
    let server = MockServer::start().await;
    mock_central(&server, first_members()).await;
    let h = host::FakeHost::start(thehome_zerotier::ZeroTier::default());
    let entry = json!({ "entry_id": "e1", "domain": "zerotier", "title": "ZeroTier",
                        "data": { "api_token": TOKEN, "api_url": server.uri(), "poll_seconds": 0.3 },
                        "options": { "networks": [NET] } });
    h.call("setup_entry", json!({ "entry": entry })).await.unwrap();

    {
        let rec = h.rec.lock().unwrap();
        // only the chosen network, and its two members
        assert_eq!(rec.devices.len(), 3);
        assert_eq!(rec.devices[0]["identifiers"], json!([["zerotier", NET]]));
        assert_eq!(rec.devices[0]["name"], "Hjem");
        assert_eq!(rec.devices[1]["identifiers"], json!([["zerotier", format!("{NET}-a1b2c3d4e5")]]));
        assert_eq!(rec.devices[1]["via_device"], json!(["zerotier", NET]));
        assert_eq!(rec.devices[1]["sw_version"], "1.14.2");
        // a member without a name is named after its address
        assert_eq!(rec.devices[2]["name"], "0f0f0f0f0f");
        let (version, _) = &rec.entities[&h_entity(&rec, &format!("{NET}-a1b2c3d4e5-version"))];
        assert_eq!(version["disabled_by_default"], true);
        assert_eq!(version["entity_category"], "diagnostic");
        let (switch, handler) = &rec.entities[&h_entity(&rec, &format!("{NET}-a1b2c3d4e5-authorized"))];
        assert!(*handler);
        assert_eq!(switch["domain"], "switch");
    }
    let laptop = |k: &str| format!("{NET}-a1b2c3d4e5-{k}");
    let phone = |k: &str| format!("{NET}-0f0f0f0f0f-{k}");
    assert_eq!(h.entity(&laptop("online")), "binary_sensor.laptop_online");

    let (s, a) = h.state_of(&laptop("online")).unwrap();
    assert_eq!(s, "on");
    assert_eq!(a["physical_address"], "203.0.113.7");
    assert_eq!(h.state_of(&phone("online")).unwrap().0, "off");
    assert_eq!(h.state_of(&laptop("authorized")).unwrap().0, "on");
    assert_eq!(h.state_of(&phone("authorized")).unwrap().0, "off");
    let (ip, a) = h.state_of(&laptop("ip")).unwrap();
    assert_eq!(ip, "10.147.17.5");
    assert_eq!(a["ip_assignments"], json!(["10.147.17.5", "fd80::5"]));
    assert!(h.state_of(&laptop("last_seen")).unwrap().0.ends_with("+00:00"));
    assert_eq!(h.state_of(&laptop("version")).unwrap().0, "1.14.2");
    let (online, a) = h.state_of(&format!("{NET}-online_members")).unwrap();
    assert_eq!(online, "1");
    assert_eq!(a["total_members"], 2);
    assert_eq!(a["network_name"], "Hjem");
    assert_eq!(h.state_of(&format!("{NET}-authorized_members")).unwrap().0, "1");

    // the switch deauthorizes through Central
    let call = |id: String, service: &str| json!({ "entry_id": "e1", "entity_id": id, "service": service, "data": {} });
    h.call("entity/call_service", call(h.entity(&laptop("authorized")), "turn_off")).await.unwrap();
    assert_eq!(h.state_of(&laptop("authorized")).unwrap().0, "off");
    assert_eq!(h.state_of(&format!("{NET}-authorized_members")).unwrap().0, "0");
    let e = h.call("entity/call_service", call(h.entity(&laptop("online")), "turn_off")).await.unwrap_err();
    assert_eq!(e["data"]["kind"], "not_supported");

    // next poll: the phone is gone and a new member joined
    server.reset().await;
    mock_central(&server, vec![member(NET, "a1b2c3d4e5", "Laptop", true, 1000), member(NET, "abcabcabca", "Server", true, 1000)]).await;
    wait_for(&h, &format!("{NET}-abcabcabca-online"), "on").await;
    wait_for(&h, &phone("online"), "unavailable").await;
    assert_eq!(h.state_of(&laptop("authorized")).unwrap().0, "on");
    wait_for(&h, &format!("{NET}-online_members"), "2").await;

    // the token is revoked: a repair issue, and everything unavailable
    server.reset().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    wait_for(&h, &laptop("online"), "unavailable").await;
    wait_for(&h, &format!("{NET}-online_members"), "unavailable").await;
    assert_eq!(h.rec.lock().unwrap().issues, ["auth"]);

    // and back
    server.reset().await;
    mock_central(&server, first_members()).await;
    wait_for(&h, &laptop("online"), "on").await;
    for _ in 0..100 {
        if h.rec.lock().unwrap().issues.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(h.rec.lock().unwrap().issues.is_empty());

    let diag = h
        .call("diagnostics", json!({ "entry": { "entry_id": "e1", "domain": "zerotier", "data": { "api_token": TOKEN }, "options": {} } }))
        .await
        .unwrap();
    assert_eq!(diag["data"]["api_token"], "**REDACTED**");
    assert!(!diag.to_string().contains(TOKEN));

    h.call("unload_entry", json!({ "entry_id": "e1" })).await.unwrap();
    let e = h.call("entity/call_service", call(h.entity(&laptop("authorized")), "turn_on")).await.unwrap_err();
    assert_eq!(e["data"]["kind"], "unavailable");
}

fn h_entity(rec: &host::Recorded, unique_id: &str) -> String {
    rec.entities.iter().find(|(_, (spec, _))| spec["unique_id"] == unique_id).map(|(k, _)| k.clone()).unwrap()
}
