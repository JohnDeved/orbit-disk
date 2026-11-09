//! Locating and talking to the running app.

use std::path::{Path, PathBuf};

use orbit_automation::catalog::{self, encode, failure_response};
use orbit_automation::transport::{self, Client};
use orbit_automation::{Failure, TransportError};
use serde_json::{Map, Value, json};

/// The per-user runtime directory holding the automation socket, from the platform layer
/// (`AppDirs::runtime`). On macOS it equals the Swift app's directory, so either build's
/// OrbitControl reaches either app.
pub fn runtime_dir() -> PathBuf {
    use orbit_platform::Platform;
    orbit_native::Native::new().app_dirs().runtime
}

/// This executable with symlinks resolved (so `~/.local/bin/orbitdisk` maps to the bundle).
pub fn executable() -> PathBuf {
    let path = std::env::current_exe().unwrap_or_else(|_| std::env::args_os().next().unwrap_or_default().into());
    std::fs::canonicalize(&path).unwrap_or(path)
}

/// `…/Name.app` when running from `Name.app/Contents/MacOS/OrbitControl`.
pub fn bundled_application() -> Option<PathBuf> {
    let executable = executable();
    let bundle = executable.parent()?.parent()?.parent()?;
    (bundle.extension()? == "app").then(|| bundle.to_path_buf())
}

pub fn server_info() -> Value {
    let version = bundled_application().and_then(|bundle| bundle_version(&bundle));
    json!({
        "name": "orbit-disk",
        "title": "Orbit Disk storage and file cleanup",
        "version": version.as_deref().unwrap_or("development"),
    })
}

/// `CFBundleShortVersionString` from the bundle's XML `Info.plist`.
fn bundle_version(bundle: &Path) -> Option<String> {
    let plist = std::fs::read_to_string(bundle.join("Contents/Info.plist")).ok()?;
    const KEY: &str = "<key>CFBundleShortVersionString</key>";
    let after_key = &plist[plist.find(KEY)? + KEY.len()..];
    let value = after_key.trim_start().strip_prefix("<string>")?;
    let value = &value[..value.find("</string>")?];
    let unescaped = value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    (!unescaped.is_empty()).then_some(unescaped)
}

/// Validates and forwards one command to the app, launching it once if it is not running.
/// Returns the response envelope.
pub fn call(command: &str, arguments: Map<String, Value>) -> Value {
    forward(command, arguments).unwrap_or_else(|failure| failure_response(&failure))
}

fn forward(command: &str, arguments: Map<String, Value>) -> Result<Value, Failure> {
    catalog::validate(command, &arguments)?;
    let request = encode(&json!({"command": command, "arguments": arguments}));
    let client = Client::new(transport::endpoint(&runtime_dir()));
    let response = match client.request(&request) {
        // Launch only after a failed connection; a request whose outcome is unknown is never replayed.
        Err(TransportError::Unavailable) => launch_and_request(&client, &request)?,
        other => other?,
    };
    let valid = |value: &Value| match value["ok"] {
        Value::Bool(true) => value["result"].is_object(),
        Value::Bool(false) => value["error"]["code"].is_string() && value["error"]["message"].is_string(),
        _ => false,
    };
    match serde_json::from_slice::<Value>(&response) {
        Ok(value) if value.is_object() && valid(&value) => Ok(value),
        _ => Err(Failure::uncertain(
            "invalid_response",
            "Orbit Disk returned an invalid response. Check status before retrying; the command may already have been accepted.",
        )),
    }
}

#[cfg(not(target_os = "macos"))]
fn launch_and_request(_client: &Client, _request: &[u8]) -> Result<Vec<u8>, Failure> {
    Err(TransportError::Unavailable.into())
}

#[cfg(target_os = "macos")]
fn launch_and_request(client: &Client, request: &[u8]) -> Result<Vec<u8>, Failure> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// How long a tool call waits for a launched app to accept connections.
    const LAUNCH_WINDOW: Duration = Duration::from_secs(10);

    let app = bundled_application().unwrap_or_else(|| PathBuf::from("/Applications/Orbit Disk.app"));
    if !app.join("Contents/MacOS/OrbitDisk").exists() {
        return Err(Failure::new(
            "app_not_installed",
            "Install Orbit Disk in Applications or open the app before calling tools.",
        ));
    }
    let deadline = Instant::now() + LAUNCH_WINDOW;
    // Diagnostics go to stderr so they never mix with MCP output on stdout.
    let mut launch = Command::new("/usr/bin/open")
        .arg("-g")
        .arg(&app)
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .spawn()
        .map_err(|error| Failure::new("operation_failed", error.to_string()))?;
    let status = loop {
        if let Some(status) = launch.try_wait().map_err(|error| Failure::new("operation_failed", error.to_string()))? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = launch.kill();
            let _ = launch.wait();
            return Err(Failure::new("launch_timeout", "Opening Orbit Disk timed out. Check the app before retrying."));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        return Err(Failure::new("launch_failed", "Could not open Orbit Disk."));
    }
    loop {
        match client.request(request) {
            Err(TransportError::Unavailable) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(TransportError::Unavailable) => {
                return Err(Failure::new(
                    "not_ready",
                    "Orbit Disk did not become ready within 10 seconds. Open the app and retry.",
                ));
            }
            other => return other.map_err(Failure::from),
        }
    }
}
