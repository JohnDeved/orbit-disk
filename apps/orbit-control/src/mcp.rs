//! MCP server: newline-delimited JSON-RPC 2.0 on stdin/stdout.
//!
//! `initialize`, `ping` and `tools/list` never contact the app; `tools/call` forwards through
//! [`crate::app::call`]. Diagnostics never go to stdout.

use std::io::Read;

use orbit_automation::catalog::{self, INSTRUCTIONS, TOOL_PREFIX, encode};
use serde_json::{Map, Value, json};

use crate::{app, emit};

pub const SUPPORTED_PROTOCOLS: [&str; 2] = ["2025-06-18", "2025-11-25"];
const MAX_LINE: usize = 1_048_576;

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Strings and integral numbers (not booleans) are valid request ids. Integral floats are
/// echoed as integers, as Foundation does.
fn request_id(id: &Value) -> Option<Value> {
    match id {
        Value::String(_) => Some(id.clone()),
        Value::Number(number) if number.is_i64() || number.is_u64() => Some(id.clone()),
        Value::Number(number) => {
            let value = number.as_f64().filter(|value| value.is_finite() && value.fract() == 0.0)?;
            if value.abs() < 9.0e15 { Some(json!(value as i64)) } else { Some(id.clone()) }
        }
        _ => None,
    }
}

#[derive(Debug, Default)]
struct Session {
    initialized: bool,
    ready: bool,
}

impl Session {
    /// Handles one line; returns the response to write, if any.
    fn process(&mut self, line: &[u8]) -> Option<Value> {
        let request = match serde_json::from_slice::<Value>(line) {
            Ok(Value::Object(request)) => request,
            Ok(_) => return Some(rpc_error(Value::Null, -32600, "Expected one JSON-RPC object.")),
            Err(_) => return Some(rpc_error(Value::Null, -32700, "Invalid JSON.")),
        };
        let id = request.get("id").and_then(request_id);
        let method = request.get("method").and_then(Value::as_str);
        let (true, Some(method)) = (request.get("jsonrpc").and_then(Value::as_str) == Some("2.0"), method) else {
            return Some(rpc_error(id.unwrap_or(Value::Null), -32600, "Invalid JSON-RPC request."));
        };
        if !request.contains_key("id") {
            // Notification: never answered.
            let params_ok = request.get("params").is_none_or(Value::is_object);
            if params_ok && method == "notifications/initialized" && self.initialized {
                self.ready = true;
            }
            return None;
        }
        let Some(id) = id else { return Some(rpc_error(Value::Null, -32600, "Invalid request ID.")) };
        let empty = Map::new();
        let params = match request.get("params") {
            None => &empty,
            Some(Value::Object(params)) => params,
            Some(_) => return Some(rpc_error(id, -32602, "Parameters must be an object.")),
        };
        let result = match method {
            "initialize" => {
                let client = params.get("clientInfo");
                let valid = !self.initialized
                    && params.get("capabilities").is_some_and(Value::is_object)
                    && client.is_some_and(|client| client["name"].is_string() && client["version"].is_string())
                    && client.is_some_and(Value::is_object);
                let Some(version) = params.get("protocolVersion").and_then(Value::as_str).filter(|_| valid) else {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "Initialization requires protocolVersion, capabilities and clientInfo with name and version.",
                    ));
                };
                self.initialized = true;
                let negotiated = SUPPORTED_PROTOCOLS.iter().find(|supported| **supported == version);
                json!({
                    "protocolVersion": negotiated.unwrap_or(&SUPPORTED_PROTOCOLS[SUPPORTED_PROTOCOLS.len() - 1]),
                    "capabilities": {"tools": {}},
                    "serverInfo": app::server_info(),
                    "instructions": INSTRUCTIONS,
                })
            }
            "ping" => json!({}),
            "tools/list" => {
                if !self.ready {
                    return Some(rpc_error(id, -32002, "Initialize first."));
                }
                if params.contains_key("cursor") {
                    return Some(rpc_error(id, -32602, "Unknown tool cursor."));
                }
                json!({"tools": catalog::tools()})
            }
            "tools/call" => {
                if !self.ready {
                    return Some(rpc_error(id, -32002, "Initialize first."));
                }
                if params.contains_key("task") {
                    return Some(rpc_error(
                        id,
                        -32602,
                        "Task execution is unsupported. Call the tool normally and poll its result state.",
                    ));
                }
                let command = params
                    .get("name")
                    .and_then(Value::as_str)
                    .and_then(|name| name.strip_prefix(TOOL_PREFIX))
                    .filter(|command| catalog::tool(command).is_some());
                let arguments = match params.get("arguments") {
                    None => Some(Map::new()),
                    Some(Value::Object(arguments)) => Some(arguments.clone()),
                    Some(_) => None,
                };
                let (Some(command), Some(arguments)) = (command, arguments) else {
                    return Some(rpc_error(id, -32602, "Unknown tool or invalid arguments."));
                };
                let response = app::call(command, arguments);
                let text = String::from_utf8_lossy(&encode(&response)).into_owned();
                let is_error = response["ok"] != true;
                json!({"content": [{"type": "text", "text": text}], "structuredContent": response, "isError": is_error})
            }
            _ => return Some(rpc_error(id, -32601, "Unknown method.")),
        };
        Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }
}

/// Serves requests until stdin closes. Lines over 1 MiB are rejected and skipped.
pub fn run() {
    let mut session = Session::default();
    let respond = |line: &[u8], session: &mut Session| {
        if let Some(response) = session.process(line) {
            emit(&response);
        }
    };
    let mut stdin = std::io::stdin().lock();
    let mut buffer = Vec::new();
    let mut skipping = false;
    let mut chunk = [0u8; 8192];
    loop {
        let count = match stdin.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        for &byte in &chunk[..count] {
            if byte == b'\n' {
                if !skipping && !buffer.is_empty() {
                    respond(&buffer, &mut session);
                }
                buffer.clear();
                skipping = false;
            } else if !skipping {
                buffer.push(byte);
                if buffer.len() > MAX_LINE {
                    emit(&rpc_error(Value::Null, -32600, "Message exceeds 1 MiB."));
                    buffer.clear();
                    skipping = true;
                }
            }
        }
    }
    if !buffer.is_empty() && !skipping {
        respond(&buffer, &mut session);
    }
}
