//! Client setup: Codex MCP registration, MCP settings for other clients and the `orbitdisk`
//! command-line symlink. Never replaces an unrelated file or server entry.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Name of the MCP server entry in client configurations.
pub const SERVER_NAME: &str = "orbit-disk";
const SUBPROCESS_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: usize = 131_072;
const DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallError {
    MissingControl,
    CodexNotFound,
    CommandFailed(String),
    InvalidConfiguration,
    ConflictingServer,
    ConflictingLink(PathBuf),
    Timeout,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingControl => f.write_str("OrbitControl is missing from this app. Reinstall Orbit Disk and try again."),
            Self::CodexNotFound => f.write_str(
                "The Codex command line tool wasn’t found. Install it, or use Copy MCP Settings to connect another client.",
            ),
            Self::CommandFailed(message) => f.write_str(message),
            Self::InvalidConfiguration => {
                f.write_str("Codex returned an unreadable server configuration. No settings were changed.")
            }
            Self::ConflictingServer => f.write_str(
                "Codex already has a different or disabled server named orbit-disk. Review that server in Codex settings before connecting.",
            ),
            Self::ConflictingLink(path) => {
                write!(f, "{} already exists and points somewhere else. No files were replaced.", path.display())
            }
            Self::Timeout => f.write_str("Codex setup timed out. Check its MCP settings before trying again."),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<InstallError> for crate::Failure {
    fn from(error: InstallError) -> Self {
        crate::Failure::new("operation_failed", error.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct IntegrationInstaller {
    control: PathBuf,
    home: PathBuf,
    environment: HashMap<OsString, OsString>,
}

struct Output {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Output {
    fn error_summary(&self) -> String {
        let end = self.stderr.len().min(500);
        String::from_utf8_lossy(&self.stderr[..end]).trim().to_owned()
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_executable(lhs: &Path, rhs: &Path) -> bool {
    canonical(lhs) == canonical(rhs)
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(metadata) = std::fs::metadata(path) else { return false };
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: `c_path` is a valid NUL-terminated path.
    !metadata.is_dir() && unsafe { libc::access(c_path.as_ptr(), libc::X_OK) } == 0
}

/// Orders `v10` after `v9` (Finder-style numeric comparison of version folder names).
fn natural_cmp(lhs: &str, rhs: &str) -> std::cmp::Ordering {
    fn chunks(text: &str) -> Vec<(bool, &str)> {
        let mut result = Vec::new();
        let mut start = 0;
        let bytes = text.as_bytes();
        for index in 1..=bytes.len() {
            if index == bytes.len() || bytes[index].is_ascii_digit() != bytes[start].is_ascii_digit() {
                result.push((bytes[start].is_ascii_digit(), &text[start..index]));
                start = index;
            }
        }
        result
    }
    let (left, right) = (chunks(lhs), chunks(rhs));
    for ((left_digits, left), (right_digits, right)) in left.iter().zip(&right) {
        let order = if *left_digits && *right_digits {
            let (l, r) = (left.trim_start_matches('0'), right.trim_start_matches('0'));
            l.len().cmp(&r.len()).then_with(|| l.cmp(r))
        } else {
            left.to_lowercase().cmp(&right.to_lowercase())
        };
        if order.is_ne() {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

impl IntegrationInstaller {
    /// `control` is the resolved path of the bundled `OrbitControl` executable.
    pub fn new(control: PathBuf, home: PathBuf, environment: HashMap<OsString, OsString>) -> Self {
        Self { control, home, environment }
    }

    /// Uses `$HOME` and the current process environment.
    pub fn for_current_user(control: PathBuf) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"));
        Self::new(control, home, std::env::vars_os().collect())
    }

    pub fn control(&self) -> &Path {
        &self.control
    }

    fn env(&self, key: &str) -> Option<&OsStr> {
        self.environment.get(OsStr::new(key)).map(OsString::as_os_str)
    }

    /// The standard `mcpServers` JSON snippet for MCP clients (pretty-printed, sorted keys).
    pub fn mcp_settings(&self) -> Result<String, InstallError> {
        self.validate_control()?;
        let value = json!({"mcpServers": {SERVER_NAME: {"command": self.control.to_string_lossy(), "args": ["mcp"]}}});
        Ok(serde_json::to_string_pretty(&value).expect("JSON values always serialize"))
    }

    /// Links `~/.local/bin/orbitdisk` to `OrbitControl`. Idempotent; refuses to replace
    /// anything that is not already a link to the same executable.
    pub fn install_command_line_tool(&self) -> Result<String, InstallError> {
        self.validate_control()?;
        let directory = self.home.join(".local/bin");
        let link = directory.join("orbitdisk");
        let conflict = || InstallError::ConflictingLink(link.clone());
        std::fs::create_dir_all(&directory).map_err(|_| conflict())?;
        match std::fs::symlink_metadata(&link) {
            Ok(metadata) => {
                if !metadata.file_type().is_symlink() || !same_executable(&link, &self.control) {
                    return Err(conflict());
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                std::os::unix::fs::symlink(&self.control, &link).map_err(|_| conflict())?;
            }
            Err(_) => return Err(conflict()),
        }
        let target = canonical(&directory);
        let on_path = self
            .env("PATH")
            .map(|path| std::env::split_paths(path).any(|entry| canonical(&entry) == target))
            .unwrap_or(false);
        Ok(if on_path {
            format!("Installed {}. Run orbitdisk --help in Terminal.", link.display())
        } else {
            format!(
                "Installed {}. Add {} to your shell’s PATH to run orbitdisk from any folder. Shell settings were not changed.",
                link.display(),
                directory.display()
            )
        })
    }

    /// Registers `OrbitControl mcp` with Codex as `orbit-disk`, verifying the result.
    pub fn connect_codex(&self) -> Result<String, InstallError> {
        self.validate_control()?;
        let codex = self.find_codex().ok_or(InstallError::CodexNotFound)?;
        let mut environment = self.environment.clone();
        let mut search = codex.parent().unwrap_or(Path::new("/")).as_os_str().to_os_string();
        search.push(":");
        search.push(self.env("PATH").unwrap_or(OsStr::new(DEFAULT_PATH)));
        environment.insert("PATH".into(), search);
        let configuration = || -> Result<Vec<Value>, InstallError> {
            let output = run(&codex, &["mcp", "list", "--json"], &environment)?;
            if !output.status.success() {
                return Err(InstallError::CommandFailed(format!(
                    "Codex couldn’t read its MCP settings. {}",
                    output.error_summary()
                )));
            }
            match serde_json::from_slice::<Value>(&output.stdout) {
                Ok(Value::Array(list))
                    if list.iter().all(|server| server["name"].is_string() && server["transport"].is_object()) =>
                {
                    Ok(list)
                }
                _ => Err(InstallError::InvalidConfiguration),
            }
        };
        let find = |servers: &[Value]| servers.iter().find(|server| server["name"] == SERVER_NAME).cloned();
        if let Some(existing) = find(&configuration()?) {
            if !self.matches(&existing) {
                return Err(InstallError::ConflictingServer);
            }
            return Ok(
                "Orbit Disk is already connected to Codex. Its tools are available in new Codex conversations.".into()
            );
        }
        let control = self.control.as_os_str();
        let output = run(
            &codex,
            &[OsStr::new("mcp"), "add".as_ref(), SERVER_NAME.as_ref(), "--".as_ref(), control, "mcp".as_ref()],
            &environment,
        )?;
        if !output.status.success() {
            return Err(InstallError::CommandFailed(format!(
                "Codex couldn’t add Orbit Disk. {}",
                output.error_summary()
            )));
        }
        match find(&configuration()?) {
            Some(entry) if self.matches(&entry) => Ok("Connected to Codex. Start a new Codex conversation to use Orbit Disk’s tools. You don’t need to mention this integration by name.".into()),
            _ => Err(InstallError::CommandFailed(
                "Codex setup finished, but the Orbit Disk connection could not be verified. Check its MCP settings.".into(),
            )),
        }
    }

    fn matches(&self, server: &Value) -> bool {
        let transport = &server["transport"];
        let command = transport["command"].as_str().unwrap_or_default();
        server["enabled"] == true
            && transport["type"] == "stdio"
            && command.starts_with('/')
            && transport["args"] == json!(["mcp"])
            && same_executable(Path::new(command), &self.control)
    }

    fn validate_control(&self) -> Result<(), InstallError> {
        if !self.control.is_absolute() || !is_executable(&self.control) {
            return Err(InstallError::MissingControl);
        }
        Ok(())
    }

    fn find_codex(&self) -> Option<PathBuf> {
        let mut candidates: Vec<PathBuf> = self
            .env("PATH")
            .map(|path| {
                std::env::split_paths(path)
                    .filter(|entry| entry.is_absolute())
                    .map(|entry| entry.join("codex"))
                    .collect()
            })
            .unwrap_or_default();
        for relative in [".local/bin/codex", ".npm-global/bin/codex"] {
            candidates.push(self.home.join(relative));
        }
        candidates.push("/opt/homebrew/bin/codex".into());
        candidates.push("/usr/local/bin/codex".into());
        if let Ok(entries) = std::fs::read_dir(self.home.join(".nvm/versions/node")) {
            let mut versions: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
            versions.sort_by(|lhs, rhs| {
                let name = |path: &PathBuf| {
                    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
                };
                natural_cmp(&name(rhs), &name(lhs))
            });
            candidates.extend(versions.into_iter().map(|version| version.join("bin/codex")));
        }
        candidates.into_iter().find(|candidate| is_executable(candidate))
    }
}

enum Event {
    Stream(usize, Vec<u8>, bool),
    Exit(std::io::Result<ExitStatus>),
}

fn signal(pid: u32, signal: libc::c_int) {
    // SAFETY: kill(2) on our own child; the waiter thread has not reaped it while we signal.
    unsafe { libc::kill(pid as libc::pid_t, signal) };
}

/// Runs `executable` with a 10 s limit and at most 128 KiB per output stream.
fn run<S: AsRef<OsStr>>(
    executable: &Path,
    arguments: &[S],
    environment: &HashMap<OsString, OsString>,
) -> Result<Output, InstallError> {
    let mut child = Command::new(executable)
        .args(arguments)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| InstallError::CommandFailed(format!("Codex couldn’t start: {error}")))?;
    let pid = child.id();
    let (sender, events) = mpsc::channel();
    let readers: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
        child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    ];
    for (stream, reader) in readers.into_iter().enumerate() {
        let Some(mut reader) = reader else { continue };
        let sender = sender.clone();
        thread::spawn(move || {
            let mut data = Vec::new();
            let mut chunk = [0u8; 16_384];
            let exceeded = loop {
                match reader.read(&mut chunk) {
                    Ok(0) => break false,
                    Ok(count) if data.len() + count > OUTPUT_LIMIT => break true,
                    Ok(count) => data.extend_from_slice(&chunk[..count]),
                    Err(error) if error.kind() == ErrorKind::Interrupted => {}
                    Err(_) => break false,
                }
            };
            let _ = sender.send(Event::Stream(stream, data, exceeded));
        });
    }
    thread::spawn(move || {
        let _ = sender.send(Event::Exit(child.wait()));
    });
    let deadline = Instant::now() + SUBPROCESS_TIMEOUT;
    let mut streams: [Option<(Vec<u8>, bool)>; 2] = [None, None];
    let record = |streams: &mut [Option<(Vec<u8>, bool)>; 2], event: Event| match event {
        Event::Stream(stream, data, exceeded) => {
            if exceeded {
                signal(pid, libc::SIGTERM);
            }
            streams[stream] = Some((data, exceeded));
            None
        }
        Event::Exit(status) => Some(status),
    };
    let status = loop {
        let Ok(event) = events.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
            signal(pid, libc::SIGTERM);
            let grace = Instant::now() + Duration::from_secs(1);
            while let Ok(event) = events.recv_timeout(grace.saturating_duration_since(Instant::now())) {
                if record(&mut streams, event).is_some() {
                    return Err(InstallError::Timeout);
                }
            }
            signal(pid, libc::SIGKILL);
            return Err(InstallError::Timeout);
        };
        if let Some(status) = record(&mut streams, event) {
            break status.map_err(|error| InstallError::CommandFailed(format!("Codex couldn’t start: {error}")))?;
        }
    };
    // Output must end within a second of exit; a grandchild holding the pipes open fails setup.
    let ended = Instant::now() + Duration::from_secs(1);
    while streams.iter().any(Option::is_none) {
        let Ok(event) = events.recv_timeout(ended.saturating_duration_since(Instant::now())) else { break };
        record(&mut streams, event);
    }
    match streams {
        [Some((stdout, false)), Some((stderr, false))] => Ok(Output { status, stdout, stderr }),
        _ => Err(InstallError::CommandFailed(
            "Codex returned too much output or left its response open. No further setup was attempted.".into(),
        )),
    }
}
