//! Protocol guards of the automation controller: request envelope, map context, item identity,
//! immutable page cursors and single-use removal tokens. Expiry uses an injected clock.

use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use orbit_automation::{
    AutomationModel, Clock, Controller, ItemInfo, ModelState, RemovalMode, RemovalReceipt, RemovalStart, ScanState,
    SessionId,
};
use serde_json::{Value, json};

#[derive(Clone)]
struct ManualClock {
    start: Instant,
    offset: Rc<Cell<Duration>>,
}

impl ManualClock {
    fn new() -> Self {
        Self { start: Instant::now(), offset: Rc::default() }
    }

    fn advance(&self, by: Duration) {
        self.offset.set(self.offset.get() + by);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Instant {
        self.start + self.offset.get()
    }

    fn wall(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000) + self.offset.get()
    }
}

fn file(name: &str, identity: &str, bytes: u64) -> ItemInfo {
    ItemInfo {
        path: format!("/map/{name}").into_bytes(),
        identity: Some(identity.into()),
        name: name.as_bytes().to_vec(),
        directory: false,
        symbolic_link: false,
        accessible: true,
        logical_bytes: bytes,
        allocated_bytes: bytes,
        file_count: 1,
        scan_state: ScanState::Complete,
        sizes_complete: true,
        can_collect: true,
    }
}

#[derive(Default)]
struct Mock {
    session: Option<SessionId>,
    state: ModelState,
    rows: Vec<ItemInfo>,
    collection: Vec<ItemInfo>,
    /// Bumped on every collection change; review tokens carry the version they saw.
    collection_version: u64,
    busy_removal: bool,
    removals: Vec<(u64, RemovalMode)>,
    receipt: Option<RemovalReceipt>,
}

impl Mock {
    fn new() -> Self {
        let rows = vec![file("big", "1:10", 300), file("medium", "1:11", 200), file("small", "1:12", 100)];
        Self { session: Some(SessionId::new_v4()), rows, ..Self::default() }
    }

    fn context(&self) -> String {
        self.session.unwrap().hyphenated().to_string().to_uppercase()
    }
}

impl AutomationModel for Mock {
    type Node = ItemInfo;
    type Review = u64;
    type Receipt = u64;

    fn session(&self) -> Option<SessionId> {
        self.session
    }
    fn state(&self) -> ModelState {
        ModelState { collection_count: self.collection.len() as u64, ..self.state.clone() }
    }
    fn root(&self) -> Option<ItemInfo> {
        None
    }
    fn current_folder(&self) -> Option<ItemInfo> {
        Some(ItemInfo { path: b"/map".to_vec(), directory: true, ..file("map", "1:1", 600) })
    }
    fn describe(&self, node: &ItemInfo) -> ItemInfo {
        node.clone()
    }
    fn displayed_rows(&self) -> Vec<ItemInfo> {
        self.rows.clone()
    }
    fn collection(&self) -> Vec<ItemInfo> {
        self.collection.clone()
    }
    fn find(&self, path: &[u8]) -> Option<ItemInfo> {
        self.rows.iter().find(|row| row.path == path).cloned()
    }
    fn removal_receipt(&self, _: &u64) -> Option<RemovalReceipt> {
        self.receipt.clone()
    }
    fn review_matches(&self, review: &u64) -> bool {
        *review == self.collection_version
    }
    fn scan(&mut self, _: &Path) {}
    fn set_query(&mut self, query: &str) {
        self.state.query = query.into();
    }
    fn retry_search(&mut self) {}
    fn open(&mut self, _: &ItemInfo) {}
    fn back(&mut self) {}
    fn select(&mut self, _: &ItemInfo) -> bool {
        true
    }
    fn reveal_selection(&mut self) {}
    fn preview_selection(&mut self) {}
    fn refresh(&mut self, _everything: bool) {}
    fn stop_scan(&mut self) {}
    fn collect(&mut self, nodes: &[ItemInfo]) -> bool {
        self.collection.extend_from_slice(nodes);
        self.collection_version += 1;
        true
    }
    fn uncollect(&mut self, node: &ItemInfo) {
        self.collection.retain(|item| item.path != node.path);
        self.collection_version += 1;
    }
    fn collection_review(&mut self) -> Option<u64> {
        (!self.collection.is_empty()).then_some(self.collection_version)
    }
    fn clear_collection(&mut self, _: &u64) {
        self.collection.clear();
        self.collection_version += 1;
    }
    fn start_removal(&mut self, review: &u64, mode: RemovalMode, _: SessionId) -> RemovalStart<u64> {
        if self.busy_removal {
            return RemovalStart::Busy;
        }
        self.removals.push((*review, mode));
        self.receipt =
            Some(RemovalReceipt { running: true, removed_count: None, message: "Removing".into(), error: None });
        RemovalStart::Started(self.removals.len() as u64)
    }
}

struct Harness {
    model: Mock,
    clock: ManualClock,
    controller: Controller<Mock, ManualClock>,
}

impl Harness {
    fn new() -> Self {
        let clock = ManualClock::new();
        Self { model: Mock::new(), controller: Controller::new(clock.clone()), clock }
    }

    fn raw(&mut self, request: &[u8]) -> Value {
        self.controller.handle(&mut self.model, request)
    }

    fn request(&mut self, command: &str, arguments: Value) -> Value {
        let request = json!({"command": command, "arguments": arguments});
        self.raw(request.to_string().as_bytes())
    }

    fn ok(&mut self, command: &str, arguments: Value) -> Value {
        let response = self.request(command, arguments.clone());
        assert_eq!(response["ok"], true, "{command} {arguments}: {response}");
        response["result"].clone()
    }

    fn error(&mut self, command: &str, arguments: Value) -> String {
        let response = self.request(command, arguments.clone());
        assert_eq!(response["ok"], false, "{command} {arguments}: {response}");
        response["error"]["code"].as_str().unwrap().to_owned()
    }

    fn collect(&mut self, names: &[&str]) {
        let context = self.model.context();
        let items: Vec<Value> = self
            .model
            .rows
            .iter()
            .filter(|row| names.iter().any(|name| row.name == name.as_bytes()))
            .map(|row| json!({"path": String::from_utf8_lossy(&row.path), "identity": row.identity}))
            .collect();
        self.ok("collect", json!({"context": context, "items": items}));
    }

    fn prepare(&mut self) -> String {
        let context = self.model.context();
        let plan = self.ok("prepare_removal", json!({"context": context, "mode": "permanent"}));
        assert_eq!(plan["requiresUserAuthorization"], true);
        plan["token"].as_str().unwrap().to_owned()
    }

    fn remove(&mut self, token: &str) -> Value {
        self.request("remove", json!({"token": token, "confirmed": true}))
    }
}

fn code(response: &Value) -> &str {
    response["error"]["code"].as_str().unwrap_or("")
}

#[test]
fn request_envelope_allows_only_command_and_arguments() {
    let mut harness = Harness::new();
    for request in [
        &br#"{"command":"status","arguments":{},"extra":1}"#[..],
        br#"{"command":"status"}"#,
        br#"{"command":"status","arguments":[]}"#,
        br#"["status"]"#,
        b"not json",
    ] {
        assert_eq!(code(&harness.raw(request)), "invalid_request", "{}", String::from_utf8_lossy(request));
    }
    assert_eq!(harness.raw(br#"{"command":"status","arguments":{}}"#)["ok"], true);
    assert_eq!(harness.error("status", json!({"extra": true})), "invalid_arguments");
}

#[test]
fn actions_require_the_current_context_and_identity() {
    let mut harness = Harness::new();
    let context = harness.model.context();
    assert_eq!(harness.ok("status", json!({}))["context"], context);
    let item = json!({"path": "/map/big", "identity": "1:10"});
    assert_eq!(harness.error("select", json!({"context": context.to_lowercase(), "item": item})), "stale_context");
    assert_eq!(
        harness.error("select", json!({"context": context, "item": {"path": "/map/big", "identity": "9:9"}})),
        "stale_item"
    );
    harness.ok("select", json!({"context": context, "item": item}));

    // While a map opens there is no context; actions carrying the previous one are rejected.
    harness.model.session = None;
    harness.model.state.opening = true;
    assert_eq!(harness.ok("status", json!({}))["context"], Value::Null);
    assert_eq!(harness.ok("list", json!({})), json!({"pending": true, "items": [], "context": null}));
    assert_eq!(harness.error("collect", json!({"context": context, "items": [item]})), "stale_context");
}

#[test]
fn page_cursors_are_immutable_snapshots_bound_to_view_and_lifetime() {
    let mut harness = Harness::new();
    let first = harness.ok("list", json!({"limit": 2}));
    assert_eq!((first["total"].clone(), first["items"][0]["path"].clone()), (json!(3), json!("/map/big")));
    let cursor = first["nextCursor"].as_str().unwrap().to_owned();

    // The snapshot is immutable: later row changes do not shift the next page.
    harness.model.rows.insert(0, file("new", "1:20", 900));
    let second = harness.ok("list", json!({"cursor": cursor}));
    assert_eq!((second["offset"].clone(), second["items"][0]["path"].clone()), (json!(2), json!("/map/small")));
    assert_eq!(second["nextCursor"], Value::Null);

    // Expires strictly after 30 s.
    harness.clock.advance(Duration::from_secs(30));
    harness.ok("list", json!({"cursor": cursor}));
    harness.clock.advance(Duration::from_millis(1));
    assert_eq!(harness.error("list", json!({"cursor": cursor})), "stale_cursor");

    // A query change invalidates the cursor; so does a collection page replacing the snapshot.
    let cursor = harness.ok("list", json!({"limit": 1}))["nextCursor"].as_str().unwrap().to_owned();
    harness.model.state.query = "big".into();
    assert_eq!(harness.error("list", json!({"cursor": cursor})), "stale_cursor");
    harness.model.state.query.clear();
    let cursor = harness.ok("list", json!({"limit": 1}))["nextCursor"].as_str().unwrap().to_owned();
    harness.ok("collection", json!({}));
    assert_eq!(harness.error("list", json!({"cursor": cursor})), "stale_cursor");
    for malformed in ["x", "x:1:2", &format!("{}:-1", cursor.split(':').next().unwrap())] {
        assert_eq!(harness.error("list", json!({"cursor": malformed})), "stale_cursor", "{malformed}");
    }
}

#[test]
fn removal_token_expires_after_five_minutes() {
    let mut harness = Harness::new();
    harness.collect(&["big"]);
    let token = harness.prepare();
    harness.clock.advance(Duration::from_secs(300));
    assert_eq!(code(&harness.remove(&token)), "invalid_plan");
    assert!(harness.model.removals.is_empty());
}

#[test]
fn removal_token_is_single_use_and_reports_its_outcome() {
    let mut harness = Harness::new();
    harness.collect(&["big", "small"]);
    let token = harness.prepare();
    assert_eq!(code(&harness.request("remove", json!({"token": token, "confirmed": false}))), "invalid_arguments");

    // A busy pipeline does not consume the plan.
    harness.model.busy_removal = true;
    assert_eq!(code(&harness.remove(&token)), "busy");
    harness.model.busy_removal = false;

    let accepted = harness.remove(&token);
    assert_eq!(
        accepted["result"],
        json!({"accepted": true, "operationID": token, "mode": "permanent", "state": "running"})
    );
    assert_eq!(harness.model.removals, [(1, RemovalMode::Permanent)]);
    assert_eq!(code(&harness.remove(&token)), "invalid_plan");
    assert_eq!(harness.model.removals.len(), 1);

    let last = &harness.ok("status", json!({}))["lastRemoval"];
    assert_eq!((last["operationID"].as_str(), last["state"].as_str()), (Some(token.as_str()), Some("running")));
    harness.model.receipt = None;
    assert_eq!(harness.ok("status", json!({}))["lastRemoval"], json!({"operationID": token, "state": "unavailable"}));
}

#[test]
fn removal_token_is_bound_to_collection_and_session() {
    let mut harness = Harness::new();
    harness.collect(&["big", "small"]);
    let token = harness.prepare();
    let context = harness.model.context();
    harness.ok("uncollect", json!({"context": context, "items": [{"path": "/map/small", "identity": "1:12"}]}));
    assert_eq!(code(&harness.remove(&token)), "stale_plan");
    // A stale plan is discarded, not merely refused.
    assert_eq!(code(&harness.remove(&token)), "invalid_plan");

    let token = harness.prepare();
    harness.model.session = Some(SessionId::new_v4());
    assert_eq!(code(&harness.remove(&token)), "stale_plan");

    // Preparing a new plan replaces the previous token.
    let first = harness.prepare();
    let second = harness.prepare();
    assert_eq!(code(&harness.remove(&first)), "invalid_plan");
    assert_eq!(harness.remove(&second)["ok"], true);
    assert_eq!(harness.model.removals.len(), 1);
}

#[test]
fn prepare_removal_refuses_more_than_200_items() {
    let mut harness = Harness::new();
    harness.model.collection = (0..201).map(|index| file(&format!("f{index}"), &format!("1:{index}"), 1)).collect();
    let context = harness.model.context();
    assert_eq!(harness.error("prepare_removal", json!({"context": context, "mode": "trash"})), "too_many_items");
}

/// `refresh` takes an optional `everything`; leaving it out must work (an absent optional key
/// once crashed the app).
#[test]
fn refresh_accepts_its_optional_everything_flag() {
    let mut harness = Harness::new();
    let context = harness.model.context();
    harness.ok("refresh", json!({"context": context}));
    harness.ok("refresh", json!({"context": context, "everything": true}));
    assert_eq!(harness.error("refresh", json!({"context": context, "everything": "yes"})), "invalid_arguments");
}
