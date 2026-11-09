//! `OrbitControl` (installed as `orbitdisk`): JSON command-line client and stdio MCP server for
//! the running Orbit Disk app.

mod app;
mod mcp;

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

use orbit_automation::catalog::{self, INSTRUCTIONS, encode, failure_response};
use orbit_automation::installer::IntegrationInstaller;
use orbit_automation::{Failure, installer};
use serde_json::{Map, Value, json};

const HELP: &str = "\
Orbit Disk: inspect disk usage, find large files, and manage cleanup using the running app.
Usage: orbitdisk COMMAND [JSON_ARGUMENTS]
       orbitdisk tools          Print command schemas
       orbitdisk --version      Print bridge/app version and supported MCP versions
       orbitdisk mcp            Start the discoverable MCP server on stdio
       orbitdisk mcp-config     Print a standard MCP client configuration
       orbitdisk connect-codex  Register automatic tool discovery in Codex
       orbitdisk install-cli    Install the ~/.local/bin/orbitdisk command
Examples:
  orbitdisk status
  orbitdisk scan '{\"path\":\"/Users/you/Downloads\"}'
  orbitdisk list '{\"limit\":20}'
Commands return JSON. Scan, search and collection validation are asynchronous.
Use status to check progress. Visible files are usable while mapping continues.
Removal requires prepare_removal followed by user-authorized remove.
";

const STANDALONE: [&str; 10] =
    ["mcp", "tools", "mcp-config", "connect-codex", "install-cli", "--version", "version", "--help", "help", "-h"];

/// Writes one compact JSON line to stdout. A closed stdout ends the process.
pub(crate) fn emit(value: &Value) {
    let mut line = encode(value);
    line.push(b'\n');
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&line).and_then(|()| stdout.flush()).is_err() {
        std::process::exit(1);
    }
}

fn emit_failure(failure: &Failure, code: u8) -> ExitCode {
    emit(&failure_response(failure));
    ExitCode::from(code)
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let first = args.first().and_then(|arg| arg.to_str());
    if let Some(command) = first.filter(|command| STANDALONE.contains(command))
        && args.len() != 1
    {
        let failure = Failure::new("invalid_arguments", format!("{command} does not accept arguments. Use --help."));
        return emit_failure(&failure, 2);
    }
    match first {
        Some("mcp") => mcp::run(),
        None | Some("--help" | "help" | "-h") => print!("{HELP}"),
        Some("--version" | "version") => {
            emit(&json!({"serverInfo": app::server_info(), "protocolVersions": mcp::SUPPORTED_PROTOCOLS}));
        }
        Some("tools") => emit(&json!({"tools": catalog::tools(), "instructions": INSTRUCTIONS})),
        Some(setup @ ("connect-codex" | "install-cli")) => {
            let installer = IntegrationInstaller::for_current_user(app::executable());
            let outcome = if setup == "connect-codex" {
                installer.connect_codex()
            } else {
                installer.install_command_line_tool()
            };
            match outcome {
                Ok(message) => emit(&json!({"ok": true, "message": message})),
                Err(error) => return emit_failure(&Failure::from(error), 1),
            }
        }
        Some("mcp-config") => emit(&json!({
            "mcpServers": {installer::SERVER_NAME: {"command": app::executable().to_string_lossy(), "args": ["mcp"]}}
        })),
        _ => return run_command(&args),
    }
    ExitCode::SUCCESS
}

/// `orbitdisk COMMAND ['{json}']`: exit 0 on success, 1 on a command failure, 2 on bad usage.
fn run_command(args: &[OsString]) -> ExitCode {
    let usage = |message: &str| Failure::new("invalid_arguments", message);
    if args.len() > 2 {
        return emit_failure(&usage("Expected one command and optional JSON object. Use --help."), 2);
    }
    let command = args[0].to_string_lossy();
    let arguments = match args.get(1) {
        None => Map::new(),
        Some(raw) => match raw.to_str().map(serde_json::from_str::<Value>) {
            Some(Ok(Value::Object(arguments))) => arguments,
            _ => return emit_failure(&usage("Arguments must be one valid JSON object."), 2),
        },
    };
    let response = app::call(&command, arguments);
    emit(&response);
    if response["ok"] == true { ExitCode::SUCCESS } else { ExitCode::from(1) }
}
