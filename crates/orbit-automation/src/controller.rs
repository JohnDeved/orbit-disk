//! The app side of the automation protocol, independent of the UI toolkit.
//!
//! Requests are `{"command": "...", "arguments": {...}}` with no other keys. Responses are
//! `{"ok":true,"result":{...}}` or `{"ok":false,"error":{...}}`.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::Failure;
use crate::catalog::{self, encode, failure_response, success_response};
use crate::model::{AutomationModel, Clock, ItemInfo, RemovalMode, RemovalStart, SessionId, SystemClock};

/// How long a list/collection snapshot serves cursors.
pub const PAGE_LIFETIME: Duration = Duration::from_secs(30);
/// How long a prepared removal token stays valid.
pub const PLAN_LIFETIME: Duration = Duration::from_secs(300);
/// Most items one removal plan may contain.
pub const MAX_PLAN_ITEMS: usize = 200;
const DEFAULT_PAGE_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageKind {
    List,
    Collection,
}

struct Page<N> {
    id: String,
    kind: PageKind,
    context: String,
    folder: Vec<u8>,
    query: String,
    created: Instant,
    created_wall: SystemTime,
    nodes: Vec<N>,
}

struct Plan<R> {
    id: String,
    review: R,
    session: SessionId,
    mode: RemovalMode,
    expires: Instant,
    expires_wall: SystemTime,
}

struct LastRemoval<R> {
    operation_id: String,
    mode: RemovalMode,
    receipt: R,
}

/// Executes automation commands against an [`AutomationModel`].
///
/// Holds at most one page snapshot (a new list or collection replaces it), at most one removal
/// plan (single-use, tied to the session and collection review) and the receipt of the last
/// automated removal.
pub struct Controller<M: AutomationModel, C: Clock = SystemClock> {
    clock: C,
    page: Option<Page<M::Node>>,
    plan: Option<Plan<M::Review>>,
    last_removal: Option<LastRemoval<M::Receipt>>,
}

impl<M: AutomationModel> Default for Controller<M> {
    fn default() -> Self {
        Self::new(SystemClock)
    }
}

impl<M: AutomationModel, C: Clock> std::fmt::Debug for Controller<M, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Controller")
            .field("page", &self.page.as_ref().map(|page| &page.id))
            .field("plan", &self.plan.as_ref().map(|plan| &plan.id))
            .finish_non_exhaustive()
    }
}

fn failure(code: &str, message: &str) -> Failure {
    Failure::new(code, message)
}

fn new_token() -> String {
    let mut buffer = uuid::Uuid::encode_buffer();
    uuid::Uuid::new_v4().hyphenated().encode_upper(&mut buffer).to_owned()
}

fn context_string(session: SessionId) -> String {
    let mut buffer = uuid::Uuid::encode_buffer();
    session.hyphenated().encode_upper(&mut buffer).to_owned()
}

/// `2026-11-09T12:34:56Z` (ISO 8601, UTC, whole seconds).
pub(crate) fn iso8601(time: SystemTime) -> String {
    let seconds = time.duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_secs() as i64).unwrap_or(0);
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

fn item_json(info: &ItemInfo) -> Value {
    json!({
        "path": String::from_utf8_lossy(&info.path),
        "identity": info.identity,
        "name": String::from_utf8_lossy(&info.name),
        "directory": info.directory,
        "symbolicLink": info.symbolic_link,
        "accessible": info.accessible,
        "logicalBytes": info.logical_bytes,
        "allocatedBytes": info.allocated_bytes,
        "fileCount": info.file_count,
        "scanState": info.scan_state.as_str(),
        "sizesComplete": info.sizes_complete,
        "canCollect": info.can_collect,
    })
}

/// Parses `{"command": string, "arguments": object}` with no other keys.
fn parse_request(data: &[u8]) -> Result<(String, Map<String, Value>), Failure> {
    let invalid = || failure("invalid_request", "Expected command and arguments.");
    let Ok(Value::Object(mut request)) = serde_json::from_slice::<Value>(data) else { return Err(invalid()) };
    if request.keys().any(|key| key != "command" && key != "arguments") {
        return Err(invalid());
    }
    match (request.remove("command"), request.remove("arguments")) {
        (Some(Value::String(command)), Some(Value::Object(arguments))) => Ok((command, arguments)),
        _ => Err(invalid()),
    }
}

impl<M: AutomationModel, C: Clock> Controller<M, C> {
    pub fn new(clock: C) -> Self {
        Self { clock, page: None, plan: None, last_removal: None }
    }

    /// Drops the page snapshot and any prepared plan (the app is closing or the server stopped).
    /// The last removal receipt is kept.
    pub fn reset(&mut self) {
        self.page = None;
        self.plan = None;
    }

    /// Handles one encoded request and returns the encoded response.
    pub fn handle_bytes(&mut self, model: &mut M, request: &[u8]) -> Vec<u8> {
        encode(&self.handle(model, request))
    }

    /// Handles one encoded request and returns the response envelope.
    pub fn handle(&mut self, model: &mut M, request: &[u8]) -> Value {
        let outcome = parse_request(request).and_then(|(command, arguments)| {
            catalog::validate(&command, &arguments)?;
            self.execute(model, &command, &arguments)
        });
        match outcome {
            Ok(result) => success_response(result),
            Err(error) => failure_response(&error),
        }
    }

    fn execute(&mut self, model: &mut M, command: &str, args: &Map<String, Value>) -> Result<Value, Failure> {
        let now = self.clock.now();
        if self.page.as_ref().is_some_and(|page| now.saturating_duration_since(page.created) > PAGE_LIFETIME) {
            self.page = None;
        }
        let text = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
        match command {
            "status" => return Ok(self.status(model)),
            "scan" => {
                let path = text("path");
                if !Path::new(path).is_absolute() {
                    return Err(failure("invalid_path", "Use an absolute folder path."));
                }
                model.scan(Path::new(path));
                return Ok(self.accepted(model));
            }
            "list" => return self.listing(model, PageKind::List, args),
            "collection" => return self.listing(model, PageKind::Collection, args),
            "remove" => return self.remove(model, args),
            _ => self.require_context(model, args)?,
        }
        let state = model.state();
        match command {
            "search" => {
                let query = text("query");
                if query == state.query {
                    model.retry_search();
                } else {
                    model.set_query(query);
                }
            }
            "back" => {
                if !state.can_go_back {
                    return Err(failure("at_root", "Already at the map root."));
                }
                model.back();
            }
            "refresh" => {
                if state.scanning || state.refreshing || state.removing {
                    return Err(failure("busy", "The map is already scanning or removing files."));
                }
                model.refresh();
            }
            "stop" => model.stop_scan(),
            "select" | "open" | "reveal" | "preview" => {
                let node = self.resolve(model, &args["item"], None)?;
                let info = model.describe(&node);
                if command == "open" {
                    if !info.directory || info.symbolic_link {
                        return Err(failure("not_directory", "Choose a regular folder from list."));
                    }
                    model.open(&node);
                } else {
                    if command == "preview" && info.directory {
                        return Err(failure("not_file", "Quick Look requires a file."));
                    }
                    if !model.select(&node) {
                        return Err(failure("stale_item", "The selection changed. List again."));
                    }
                    match command {
                        "reveal" => model.reveal_selection(),
                        "preview" => model.preview_selection(),
                        _ => {}
                    }
                }
            }
            "collect" => {
                if state.removing {
                    return Err(failure("busy", "Files are being removed."));
                }
                let nodes = self.resolve_all(model, &args["items"], None)?;
                if !nodes.iter().all(|node| model.describe(node).can_collect) || !model.collect(&nodes) {
                    return Err(failure(
                        "not_collectable",
                        "These items cannot be collected. Check collection and list again.",
                    ));
                }
            }
            "uncollect" => {
                Self::require_collection_ready(model)?;
                let collection = model.collection();
                for node in self.resolve_all(model, &args["items"], Some(&collection))? {
                    model.uncollect(&node);
                }
            }
            "clear_collection" => {
                Self::require_collection_ready(model)?;
                if !model.collection().is_empty() {
                    let review = Self::review_token(model)?;
                    model.clear_collection(&review);
                }
            }
            "prepare_removal" => return self.prepare_removal(model, text("mode")),
            _ => return Err(failure("unknown_command", "Unknown command.")),
        }
        Ok(self.accepted(model))
    }

    fn prepare_removal(&mut self, model: &mut M, mode: &str) -> Result<Value, Failure> {
        Self::require_collection_ready(model)?;
        let collection = model.collection();
        if collection.len() > MAX_PLAN_ITEMS {
            return Err(failure(
                "too_many_items",
                "Automated removal supports up to 200 items per review. Reduce the collection or use the app's review.",
            ));
        }
        let review = Self::review_token(model)?;
        let session = model.session().ok_or_else(|| failure("stale_context", "The active map changed."))?;
        let mode = RemovalMode::parse(mode).ok_or_else(|| failure("invalid_arguments", "Invalid arguments.mode."))?;
        let plan = Plan {
            id: new_token(),
            review,
            session,
            mode,
            expires: self.clock.now() + PLAN_LIFETIME,
            expires_wall: self.clock.wall() + PLAN_LIFETIME,
        };
        let state = model.state();
        let result = json!({
            "token": plan.id,
            "mode": mode.as_str(),
            "expiresAt": iso8601(plan.expires_wall),
            "items": collection.iter().map(|node| item_json(&model.describe(node))).collect::<Vec<_>>(),
            "count": collection.len(),
            "logicalBytes": state.collection_bytes,
            "sizesComplete": !state.partial_map && !state.updating_sizes,
            "requiresUserAuthorization": true,
        });
        self.plan = Some(plan);
        Ok(result)
    }

    fn remove(&mut self, model: &mut M, args: &Map<String, Value>) -> Result<Value, Failure> {
        let token = args.get("token").and_then(Value::as_str);
        let confirmed = args.get("confirmed").and_then(Value::as_bool) == Some(true);
        let now = self.clock.now();
        let Some(plan) =
            self.plan.as_ref().filter(|plan| Some(plan.id.as_str()) == token && plan.expires > now && confirmed)
        else {
            return Err(failure("invalid_plan", "Prepare a new removal plan and authorize its exact items and mode."));
        };
        if model.session() != Some(plan.session) || !model.review_matches(&plan.review) {
            self.plan = None;
            return Err(failure("stale_plan", "The map or collection changed. Prepare a new removal plan."));
        }
        Self::require_collection_ready(model)?;
        let started = model.start_removal(&plan.review, plan.mode, plan.session);
        if matches!(started, RemovalStart::Busy) {
            return Err(failure("busy", "A removal or confirmation is already in progress."));
        }
        let plan = self.plan.take().expect("plan checked above");
        let RemovalStart::Started(receipt) = started else {
            return Err(failure("not_started", "The removal was not started. Review the collection again."));
        };
        let result = json!({"accepted": true, "operationID": plan.id, "mode": plan.mode.as_str(), "state": "running"});
        self.last_removal = Some(LastRemoval { operation_id: plan.id, mode: plan.mode, receipt });
        Ok(result)
    }

    fn listing(&mut self, model: &mut M, kind: PageKind, args: &Map<String, Value>) -> Result<Value, Failure> {
        let state = model.state();
        if kind == PageKind::List && state.filtering && state.search_failed && !state.searching {
            return Err(failure("search_failed", "Search could not finish. Retry search or refresh the map."));
        }
        let limit = args.get("limit").and_then(Value::as_f64).map_or(DEFAULT_PAGE_LIMIT, |limit| limit as usize);
        let context = model.session().map(context_string);
        let folder = model.current_folder().map(|node| model.describe(&node).path).unwrap_or_default();
        let offset = if let Some(cursor) = args.get("cursor").and_then(Value::as_str) {
            let stale =
                || failure("stale_cursor", "The page expired or the view changed. List again without a cursor.");
            let mut parts = cursor.split(':');
            let (Some(id), Some(index), None) = (parts.next(), parts.next(), parts.next()) else { return Err(stale()) };
            let index: usize = index.parse().map_err(|_| stale())?;
            let page = self.page.as_ref().ok_or_else(stale)?;
            let current = page.id == id
                && page.kind == kind
                && Some(&page.context) == context.as_ref()
                && page.folder == folder
                && page.query == state.query
                && index <= page.nodes.len();
            if !current {
                return Err(stale());
            }
            index
        } else {
            let Some(context) = context else {
                return Ok(json!({"pending": state.opening, "items": [], "context": null}));
            };
            if kind == PageKind::List
                && (state.sorting_rows || (state.filtering && (state.searching || state.search_outdated)))
            {
                return Ok(json!({
                    "pending": true, "items": [], "context": context,
                    "query": state.query, "searchFailed": state.search_failed,
                }));
            }
            let nodes = match kind {
                PageKind::List => model.displayed_rows(),
                PageKind::Collection => model.collection(),
            };
            self.page = Some(Page {
                id: new_token(),
                kind,
                context,
                folder,
                query: state.query.clone(),
                created: self.clock.now(),
                created_wall: self.clock.wall(),
                nodes,
            });
            0
        };
        let page = self.page.as_ref().expect("page selected above");
        let total = page.nodes.len();
        let end = offset + limit.min(total - offset);
        let mut result = json!({
            "items": page.nodes[offset..end].iter().map(|node| item_json(&model.describe(node))).collect::<Vec<_>>(),
            "context": page.context,
            "folder": String::from_utf8_lossy(&page.folder),
            "query": page.query,
            "snapshotAt": iso8601(page.created_wall),
            "total": total,
            "offset": offset,
            "nextCursor": if end < total { Value::String(format!("{}:{end}", page.id)) } else { Value::Null },
            "pending": kind == PageKind::Collection && (state.validating_collection || state.updating_collection),
            "mappingIncomplete": state.partial_map || state.scanning,
        });
        if kind == PageKind::List {
            result["searchMatchCount"] = json!(if state.filtering { state.search_match_count } else { total as u64 });
            result["searchTruncated"] = json!(state.filtering && state.search_match_count > total as u64);
        }
        Ok(result)
    }

    fn require_context(&self, model: &M, args: &Map<String, Value>) -> Result<(), Failure> {
        let expected = model.session().map(context_string);
        match (expected, args.get("context").and_then(Value::as_str)) {
            (Some(expected), Some(given)) if expected == given => Ok(()),
            _ => Err(failure("stale_context", "The active map changed or is opening. Read status/list again.")),
        }
    }

    fn require_collection_ready(model: &M) -> Result<(), Failure> {
        let state = model.state();
        if state.removing || state.validating_collection || state.updating_collection {
            return Err(failure("busy", "The collection is still changing. Poll collection before continuing."));
        }
        Ok(())
    }

    fn review_token(model: &mut M) -> Result<M::Review, Failure> {
        model.collection_review().ok_or_else(|| failure("empty_collection", "The collection is empty or unavailable."))
    }

    /// Resolves `{path, identity}` in the active map, or within `collection` when given.
    fn resolve(&self, model: &M, item: &Value, collection: Option<&[M::Node]>) -> Result<M::Node, Failure> {
        let stale = || failure("stale_item", "The item is no longer in this view or its identity changed. List again.");
        let path = item["path"].as_str().unwrap_or_default().as_bytes();
        let identity = item["identity"].as_str();
        let node = match collection {
            Some(collection) => collection.iter().find(|node| model.describe(node).path == path).cloned(),
            None => model.find(path),
        }
        .ok_or_else(stale)?;
        let info = model.describe(&node);
        if info.path != path || info.identity.as_deref() != identity {
            return Err(stale());
        }
        Ok(node)
    }

    fn resolve_all(&self, model: &M, items: &Value, collection: Option<&[M::Node]>) -> Result<Vec<M::Node>, Failure> {
        items.as_array().into_iter().flatten().map(|item| self.resolve(model, item, collection)).collect()
    }

    fn status(&self, model: &M) -> Value {
        let state = model.state();
        let describe = |node: Option<M::Node>| node.map_or(Value::Null, |node| item_json(&model.describe(&node)));
        let mut value = json!({
            "context": model.session().map(context_string),
            "root": describe(model.root()),
            "folder": describe(model.current_folder()),
            "opening": state.opening,
            "scanning": state.scanning,
            "refreshing": state.refreshing,
            "mappingIncomplete": state.partial_map,
            "discoveredFiles": state.discovered_files,
            "discoveredBytes": state.discovered_bytes,
            "query": state.query,
            "searching": state.searching,
            "searchOutdated": state.search_outdated,
            "searchFailed": state.search_failed,
            "collectionCount": state.collection_count,
            "collectionBytes": state.collection_bytes,
            "collectionPending": state.validating_collection || state.updating_collection,
            "removing": state.active_removals,
            "updatingSizes": state.updating_sizes,
            "message": state.message,
            "error": state.error,
        });
        if let Some(last) = &self.last_removal {
            value["lastRemoval"] = match model.removal_receipt(&last.receipt) {
                Some(receipt) => json!({
                    "operationID": last.operation_id,
                    "mode": last.mode.as_str(),
                    "state": if receipt.running { "running" } else { "finished" },
                    "removedCount": receipt.removed_count,
                    "message": receipt.message,
                    "error": receipt.error,
                }),
                None => json!({"operationID": last.operation_id, "state": "unavailable"}),
            };
        }
        value
    }

    fn accepted(&self, model: &M) -> Value {
        json!({"accepted": true, "status": self.status(model)})
    }
}

#[cfg(test)]
mod tests {
    use super::iso8601;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn iso8601_matches_foundation_formatter() {
        assert_eq!(iso8601(UNIX_EPOCH), "1970-01-01T00:00:00Z");
        // 2024-02-29T23:59:59Z: leap day, end of day.
        assert_eq!(iso8601(UNIX_EPOCH + Duration::from_secs(1_709_251_199)), "2024-02-29T23:59:59Z");
        assert_eq!(iso8601(UNIX_EPOCH + Duration::from_secs(4_102_444_800)), "2100-01-01T00:00:00Z");
    }
}
