//! Client setup never replaces unrelated files or MCP entries, and unreadable Codex output never
//! leads to a registration. Uses a fake `codex` script (requires python3).
#![cfg(unix)]

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use orbit_automation::installer::{InstallError, IntegrationInstaller};
use serde_json::{Value, json};

const FAKE_CODEX: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, sys
root = pathlib.Path(os.environ['FAKE_CODEX_ROOT'])
args = sys.argv[1:]
with (root / 'calls').open('a') as calls:
    calls.write(json.dumps(args) + '\n')
mode = os.environ.get('FAKE_CODEX_MODE', '')
if mode == 'empty': sys.exit(0)
if mode == 'invalid': print('not json'); sys.exit(0)
if mode == 'large': print('x' * 300000); sys.exit(0)
state = root / 'servers.json'
servers = json.loads(state.read_text())
if args == ['mcp', 'list', '--json']:
    print(json.dumps(servers))
elif args[:4] == ['mcp', 'add', 'orbit-disk', '--']:
    assert len(args) == 6 and args[5] == 'mcp'
    assert not any(s['name'] == 'orbit-disk' for s in servers)
    servers.append({'name': 'orbit-disk', 'enabled': True, 'transport': {'type': 'stdio', 'command': args[4], 'args': ['mcp']}})
    state.write_text(json.dumps(servers))
    print('Added')
else:
    sys.exit(2)
"#;

fn executable(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
    std::fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    control: PathBuf,
    home: PathBuf,
    state: PathBuf,
    environment: HashMap<OsString, OsString>,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let control = root.join("My App.app/Contents/MacOS/OrbitControl");
    executable(&control, "#!/bin/sh\nexit 0\n");
    executable(&root.join("bin/codex"), FAKE_CODEX);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let state = root.join("servers.json");
    let other = json!([{"name": "another-server", "transport": {"type": "stdio", "command": "keep", "args": []}, "enabled": true}]);
    std::fs::write(&state, other.to_string()).unwrap();
    let path = format!("{}:/usr/bin:/bin", root.join("bin").display());
    let environment = HashMap::from([("PATH".into(), path.into()), ("FAKE_CODEX_ROOT".into(), root.clone().into())]);
    Fixture { _dir: dir, root, control, home, state, environment }
}

impl Fixture {
    fn installer(&self, mode: Option<&str>) -> IntegrationInstaller {
        let mut environment = self.environment.clone();
        if let Some(mode) = mode {
            environment.insert("FAKE_CODEX_MODE".into(), mode.into());
        }
        IntegrationInstaller::new(self.control.clone(), self.home.clone(), environment)
    }

    fn servers(&self) -> Vec<Value> {
        serde_json::from_slice::<Value>(&std::fs::read(&self.state).unwrap()).unwrap().as_array().unwrap().clone()
    }
}

#[test]
fn command_line_link_is_idempotent_and_never_replaces_files() {
    let fixture = fixture();
    let installer = fixture.installer(None);
    let settings: Value = serde_json::from_str(&installer.mcp_settings().unwrap()).unwrap();
    assert_eq!(settings["mcpServers"]["orbit-disk"], json!({"command": fixture.control.to_str(), "args": ["mcp"]}));

    assert!(installer.install_command_line_tool().unwrap().contains("PATH"), "PATH guidance when not on PATH");
    let link = fixture.home.join(".local/bin/orbitdisk");
    assert_eq!(std::fs::canonicalize(&link).unwrap(), fixture.control);
    installer.install_command_line_tool().unwrap();
    std::fs::remove_file(&link).unwrap();
    std::fs::write(&link, b"keep").unwrap();
    assert!(matches!(installer.install_command_line_tool(), Err(InstallError::ConflictingLink(_))));
    assert_eq!(std::fs::read(&link).unwrap(), b"keep");
}

#[test]
fn codex_registration_adds_once_and_preserves_conflicts() {
    let fixture = fixture();
    let installer = fixture.installer(None);
    installer.connect_codex().unwrap();
    installer.connect_codex().unwrap();
    let calls = std::fs::read_to_string(fixture.root.join("calls")).unwrap();
    assert_eq!(calls.matches("\"add\"").count(), 1, "repeated connection adds exactly once");
    let mut servers = fixture.servers();
    assert_eq!((servers.len(), servers[0]["name"].as_str()), (2, Some("another-server")));

    // An entry pointing at a symlink to the same executable counts as connected.
    let alias = fixture.root.join("control-alias");
    symlink(&fixture.control, &alias).unwrap();
    servers[1]["transport"]["command"] = json!(alias);
    std::fs::write(&fixture.state, Value::from(servers.clone()).to_string()).unwrap();
    installer.connect_codex().unwrap();

    servers[1]["transport"]["command"] = json!("/different/app/OrbitControl");
    std::fs::write(&fixture.state, Value::from(servers).to_string()).unwrap();
    let before = std::fs::read(&fixture.state).unwrap();
    assert_eq!(installer.connect_codex(), Err(InstallError::ConflictingServer));
    assert_eq!(std::fs::read(&fixture.state).unwrap(), before);

    for mode in ["empty", "invalid", "large"] {
        let result = fixture.installer(Some(mode)).connect_codex();
        assert!(
            matches!(result, Err(InstallError::InvalidConfiguration | InstallError::CommandFailed(_))),
            "{mode}: {result:?}"
        );
        assert_eq!(std::fs::read(&fixture.state).unwrap(), before, "{mode} never triggers an add");
    }
}
