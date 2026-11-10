//! `cargo xtask bench-compare`: repeated fresh-process runs of the `orbit-bench` probes.
//!
//! Builds the probes (release profile), then runs every suite for `--rounds` rounds:
//!
//! - `indexing`: `indexing_probe` through `scripts/benchmarks/run-durable-indexing.py` (and its
//!   summarizer).
//! - `marking`: `marking_probe` (3 timed passes per process).
//! - `pipeline`: `pipeline_probe` through `scripts/benchmarks/run-indexing-pipeline.py`.
//! - `discovery`: `discovery_probe sample` on generated flat and mixed fixtures, 1/4/8 workers
//!   (4 with `--small`), with CPU and RSS.
//! - `e2e`: `e2e_probe` on generated trees.
//!
//! Writes `results.jsonl` (one line per sample, with the probe's raw output), `summary.md`
//! (median/min/max) and per-run stderr under `raw/`. Generated fixtures live in a private
//! directory under `target/xtask` that is deleted afterwards. Reports, not timing gates. To
//! compare two builds, run each and compare the summaries, or pass both builds' probes to the
//! Python runners (`--binary=label=path`, repeatable), which interleave them.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::{Args, Result, exec, target_dir, workspace_root};

const USAGE: &str = "Usage: cargo xtask bench-compare --output NEW-DIR [--rounds N] \
                     [--suites indexing,marking,pipeline,discovery,e2e] [--small]";
const SUITES: [&str; 5] = ["indexing", "marking", "pipeline", "discovery", "e2e"];

#[derive(Debug)]
struct Options {
    rounds: u32,
    output: PathBuf,
    suites: Vec<String>,
    small: bool,
}

fn parse(args: &[OsString]) -> Result<Options> {
    let (mut rounds, mut output, mut suites, mut small) = (3, None, None, false);
    let mut args = Args::new(args);
    while let Some(arg) = args.next() {
        match arg? {
            "--rounds" => {
                let value = args.value("--rounds")?;
                let parsed = value.to_str().and_then(|value| value.parse::<u32>().ok()).filter(|&value| value > 0);
                rounds = parsed.ok_or_else(|| format!("--rounds needs a positive integer, got {value:?}"))?;
            }
            "--swift-dir" => {
                return Err(
                    "--swift-dir was removed with the Swift v0.9.15 probes; the suites run the Rust probes".into()
                );
            }
            "--output" => output = Some(PathBuf::from(args.value("--output")?)),
            "--suites" => {
                let value = args.value("--suites")?.into_string().map_err(|_| "--suites must be UTF-8")?;
                let list: Vec<String> = value.split(',').map(str::to_string).collect();
                if let Some(unknown) = list.iter().find(|suite| !SUITES.contains(&suite.as_str())) {
                    return Err(format!("unknown suite {unknown:?}; choose from {}", SUITES.join(",")));
                }
                suites = Some(list);
            }
            "--small" => small = true,
            other => return Err(format!("unexpected argument {other:?}\n{USAGE}")),
        }
    }
    Ok(Options {
        rounds,
        output: output.ok_or(format!("--output is required\n{USAGE}"))?,
        suites: suites.unwrap_or_else(|| SUITES.map(str::to_string).to_vec()),
        small,
    })
}

/// One measured value.
struct Sample {
    suite: &'static str,
    scenario: String,
    label: &'static str,
    round: u32,
    metric: &'static str,
    unit: &'static str,
    value: f64,
    raw: Value,
}

struct Run {
    options: Options,
    output: PathBuf,
    work: WorkDir,
    samples: Vec<Sample>,
    notes: Vec<String>,
    jsonl: fs::File,
}

pub fn run(args: &[OsString]) -> Result<()> {
    let options = parse(args)?;
    let output = std::path::absolute(&options.output).map_err(|error| format!("invalid --output: {error}"))?;
    if fs::symlink_metadata(&output).is_ok() {
        return Err(format!("{} already exists; bench-compare writes into a new directory", output.display()));
    }

    let root = workspace_root();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    exec(Command::new(cargo).current_dir(&root).args([
        "build",
        "--locked",
        "--release",
        "-p",
        "orbit-bench",
        "--bins",
    ]))?;
    let rust_dir = target_dir().join("release");

    fs::create_dir_all(output.join("raw"))
        .map_err(|error| format!("could not create {}: {error}", output.display()))?;
    let jsonl =
        fs::File::create_new(output.join("results.jsonl")).map_err(|error| format!("results.jsonl: {error}"))?;
    let mut run =
        Run { work: WorkDir::new()?, output: output.clone(), samples: Vec::new(), notes: Vec::new(), jsonl, options };
    let mut environment = environment(&root)?;
    eprintln!("bench-compare: load average before: {}", environment["load_before"].as_str().unwrap_or("?"));

    let suites = run.options.suites.clone();
    let mut loads = Vec::new();
    for suite in &suites {
        let before = load_average();
        eprintln!("bench-compare: suite {suite} (load {before})");
        match suite.as_str() {
            "indexing" => run.indexing(&rust_dir.join("indexing_probe"))?,
            "marking" => run.marking(&rust_dir.join("marking_probe"))?,
            "pipeline" => run.pipeline(&rust_dir.join("pipeline_probe"))?,
            "discovery" => run.discovery(&rust_dir.join("discovery_probe"))?,
            "e2e" => run.e2e(&rust_dir.join("e2e_probe"))?,
            _ => unreachable!("validated in parse"),
        }
        loads.push(json!({"suite": suite, "before": before, "after": load_average()}));
    }
    environment["load_after"] = json!(load_average());
    environment["suite_loads"] = json!(loads);
    fs::write(output.join("environment.json"), serde_json::to_string_pretty(&environment).expect("json") + "\n")
        .map_err(|error| format!("environment.json: {error}"))?;
    let summary = run.summary(&environment);
    fs::write(output.join("summary.md"), &summary).map_err(|error| format!("summary.md: {error}"))?;
    print!("{summary}");
    eprintln!("bench-compare: wrote {}", output.display());
    Ok(())
}

// --- Suites ------------------------------------------------------------------------------------

impl Run {
    fn record(&mut self, sample: Sample) -> Result<()> {
        let line = json!({
            "suite": sample.suite, "scenario": sample.scenario, "label": sample.label, "round": sample.round,
            "metric": sample.metric, "unit": sample.unit, "value": sample.value, "raw": sample.raw,
        });
        writeln!(self.jsonl, "{line}").map_err(|error| format!("results.jsonl: {error}"))?;
        self.samples.push(sample);
        Ok(())
    }

    fn raw_dir(&self, suite: &str) -> Result<PathBuf> {
        let dir = self.output.join("raw").join(suite);
        fs::create_dir_all(&dir).map_err(|error| format!("could not create {}: {error}", dir.display()))?;
        Ok(dir)
    }

    /// Runs a probe in a fresh process; keeps its stderr under `raw/<suite>/<name>.stderr`.
    fn probe(&self, suite: &str, name: &str, command: &mut Command) -> Result<String> {
        let output = command.output().map_err(|error| format!("could not start {command:?}: {error}"))?;
        fs::write(self.raw_dir(suite)?.join(format!("{name}.stderr")), &output.stderr)
            .map_err(|error| format!("could not keep stderr: {error}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let tail = &stderr[stderr.len().saturating_sub(2000)..];
            return Err(format!("{name} failed with {}: {tail}", output.status));
        }
        String::from_utf8(output.stdout).map_err(|_| format!("{name} printed non-UTF-8 output"))
    }

    /// The durable-indexing runner, with its databases in the work dir.
    fn indexing(&mut self, rust: &Path) -> Result<()> {
        let shapes: &[&str] = if self.options.small {
            &["small-g0", "small-g1", "empty-g0", "empty-g1"]
        } else {
            &["flat-million", "small-g0", "small-g1", "empty-g0", "empty-g1"]
        };
        let work = self.work.0.join("indexing");
        let root = workspace_root();
        exec(
            Command::new("python3")
                .arg(root.join("scripts/benchmarks/run-durable-indexing.py"))
                .arg(format!("--binary=rust={}", rust.display()))
                .args(["--rounds", &self.options.rounds.to_string(), "--output"])
                .arg(&work)
                .arg("--shapes")
                .args(shapes),
        )?;
        let raw = self.raw_dir("indexing")?;
        keep_runner_output(&work, &raw)?;
        exec(Command::new("python3").arg(root.join("scripts/benchmarks/summarize-durable-indexing.py")).arg(&raw))?;
        let _ = fs::remove_dir_all(&work);
        for row in read_jsonl(&raw.join("results.jsonl"))? {
            let scenario = row["scenario"].as_str().unwrap_or("?").to_string();
            let label = label(&row)?;
            let round = row["round"].as_u64().unwrap_or(0) as u32;
            let ingestion = number(&row, "ingestion_seconds")?;
            let publication = row["phase_seconds"]["publication"].as_f64().unwrap_or(0.0);
            let wal = row["wal_max_sampled_bytes"].as_f64().unwrap_or(0.0) / 1_048_576.0;
            for (metric, unit, value) in [
                ("ingestion", "s", ingestion),
                ("ingestion + publication", "s", ingestion + publication),
                ("peak WAL", "MiB", wal),
            ] {
                self.record(Sample {
                    suite: "indexing",
                    scenario: scenario.clone(),
                    label,
                    round,
                    metric,
                    unit,
                    value,
                    raw: row.clone(),
                })?;
            }
        }
        Ok(())
    }

    fn marking(&mut self, rust: &Path) -> Result<()> {
        let counts: &[u32] = if self.options.small { &[50_000] } else { &[50_000, 100_000] };
        for round in 1..=self.options.rounds {
            for &count in counts {
                {
                    let (label, probe) = ("rust", rust);
                    let name = format!("r{round}-{count}-{label}");
                    let dir = self.work.0.join("marking").join(&name);
                    fs::create_dir_all(dir.parent().expect("has parent")).map_err(|error| error.to_string())?;
                    let stdout =
                        self.probe("marking", &name, Command::new(probe).arg(&dir).arg(count.to_string()).arg("3"))?;
                    let _ = fs::remove_dir_all(&dir);
                    let result: Value =
                        serde_json::from_str(stdout.trim()).map_err(|error| format!("{name}: {error}"))?;
                    let passes = result["marking"].as_array().cloned().unwrap_or_default();
                    for pass in passes {
                        let value = number(&pass, "seconds")?;
                        let scenario = format!("{}k directories", count / 1000);
                        self.record(Sample {
                            suite: "marking",
                            scenario,
                            label,
                            round,
                            metric: "mark full refresh",
                            unit: "s",
                            value,
                            raw: pass,
                        })?;
                    }
                }
            }
        }
        Ok(())
    }

    /// The pipeline runner (its fixture and caches live in its own `/private/tmp` dir).
    fn pipeline(&mut self, rust: &Path) -> Result<()> {
        let shapes: &[u32] = if self.options.small { &[1000] } else { &[1000, 2000] };
        for &directories in shapes {
            let scenario = format!("{directories} folders x 10 files");
            let raw = self.raw_dir("pipeline")?.join(format!("{directories}-folders"));
            exec(
                Command::new("python3")
                    .arg(workspace_root().join("scripts/benchmarks/run-indexing-pipeline.py"))
                    .arg(format!("--binary=rust={}", rust.display()))
                    .args(["--rounds", &self.options.rounds.to_string(), "--directories", &directories.to_string()])
                    .arg("--output")
                    .arg(&raw),
            )?;
            for row in read_jsonl(&raw.join("results.jsonl"))? {
                let label = label(&row)?;
                let round = row["round"].as_u64().unwrap_or(0) as u32;
                for (metric, key) in [
                    ("total", "total_seconds"),
                    ("discovery", "discovery_seconds"),
                    ("ready to publish", "mapping_ready_seconds"),
                ] {
                    let value = number(&row, key)?;
                    self.record(Sample {
                        suite: "pipeline",
                        scenario: scenario.clone(),
                        label,
                        round,
                        metric,
                        unit: "s",
                        value,
                        raw: row.clone(),
                    })?;
                }
            }
        }
        Ok(())
    }

    /// `discovery_probe sample` on generated fixtures (flat, mixed), 1/4/8 workers, one fresh
    /// process per sample.
    fn discovery(&mut self, rust: &Path) -> Result<()> {
        let dir = self.work.0.join("discovery");
        self.probe("discovery", "fixtures", Command::new(rust).arg("fixtures").arg("--dir").arg(&dir))?;
        let fixtures = ["flat", "mixed"];
        let workers: &[u32] = if self.options.small { &[4] } else { &[1, 4, 8] };
        let command = |fixture: &str, workers: u32| -> Command {
            let mut command = Command::new(rust);
            command.arg("sample").arg("--dir").arg(&dir).args([
                "--fixture",
                fixture,
                "--workers",
                &workers.to_string(),
            ]);
            command
        };
        // Unmeasured warm-up, as in `discovery_probe bench`.
        for fixture in fixtures {
            self.probe("discovery", &format!("warmup-{fixture}"), &mut command(fixture, 4))?;
        }
        for round in 1..=self.options.rounds {
            for fixture in fixtures {
                for index in 0..workers.len() {
                    // Rotate the worker order too, so no count always runs first.
                    let count = workers[(index + round as usize) % workers.len()];
                    {
                        let label = "rust";
                        let name = format!("r{round}-{fixture}-w{count}");
                        let load = load_average();
                        let stdout = self.probe("discovery", &name, &mut command(fixture, count))?;
                        let mut raw = discovery_output(&stdout);
                        if raw["correct"] != json!(true) && raw["correct"] != json!("true") {
                            return Err(format!("{name}: incorrect inventory: {stdout}"));
                        }
                        raw["load_before"] = json!(load);
                        let value = |key: &str| -> Result<f64> {
                            raw[key]
                                .as_f64()
                                .or_else(|| raw[key].as_str().and_then(|v| v.parse().ok()))
                                .ok_or_else(|| format!("{name}: no {key} in {stdout}"))
                        };
                        let scenario = format!("{fixture} ({count} worker{})", if count == 1 { "" } else { "s" });
                        for (metric, unit, number) in [
                            ("discovery", "s", value("discovery_ms")? / 1e3),
                            ("user CPU", "s", value("user_ms")? / 1e3),
                            ("system CPU", "s", value("system_ms")? / 1e3),
                            ("peak RSS", "MiB", value("peak_rss")? / 1_048_576.0),
                        ] {
                            self.record(Sample {
                                suite: "discovery",
                                scenario: scenario.clone(),
                                label,
                                round,
                                metric,
                                unit,
                                value: number,
                                raw: raw.clone(),
                            })?;
                        }
                    }
                }
            }
        }
        let _ = fs::remove_dir_all(&dir);
        self.notes.push(
            "discovery: discovery into the live inventory only (no writer). CPU rows are process user/system time \
             over the same span (see docs/benchmarks/rust-port/discovery-scaling.md)."
                .into(),
        );
        Ok(())
    }

    fn e2e(&mut self, rust: &Path) -> Result<()> {
        let sizes: &[u32] = if self.options.small { &[20_000] } else { &[20_000, 180_000] };
        for &files in sizes {
            let dir = self.work.0.join(format!("e2e-{files}"));
            self.probe(
                "e2e",
                &format!("fixture-{files}"),
                Command::new(rust).arg("fixture").arg("--dir").arg(&dir).args(["--files", &files.to_string()]),
            )?;
            let scenario = format!("{}k files", files / 1000);
            for round in 1..=self.options.rounds {
                let name = format!("r{round}-{files}-rust");
                let stdout = self.probe("e2e", &name, Command::new(rust).arg("run").arg("--dir").arg(&dir))?;
                let result: Value = serde_json::from_str(stdout.trim()).map_err(|error| format!("{name}: {error}"))?;
                for (metric, key, unit, scale) in [
                    ("total to publication", "total_to_publish_ms", "s", 1e-3),
                    ("discovery", "discovery_ms", "s", 1e-3),
                    ("first full view", "first_full_view_ms", "s", 1e-3),
                    ("peak RSS", "peak_rss_mib", "MiB", 1.0),
                ] {
                    let value = number(&result, key)? * scale;
                    self.record(Sample {
                        suite: "e2e",
                        scenario: scenario.clone(),
                        label: "rust",
                        round,
                        metric,
                        unit,
                        value,
                        raw: result.clone(),
                    })?;
                }
            }
            let _ = fs::remove_dir_all(&dir);
        }
        self.notes.push("e2e: a whole mapping session on a generated tree (sparse files).".into());
        Ok(())
    }

    // --- Summary -------------------------------------------------------------------------------

    fn summary(&self, environment: &Value) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# `cargo xtask bench-compare`\n");
        let field = |key: &str| environment[key].as_str().unwrap_or("?").to_string();
        let _ = writeln!(
            out,
            "Measured {} on {} ({} cores, {} GiB, macOS {}), commit `{}`, {}.\n",
            field("date"),
            field("cpu"),
            environment["cores"],
            environment["memory_gib"],
            field("macos"),
            field("commit"),
            field("rustc"),
        );
        let _ = writeln!(
            out,
            "{} fresh-process round(s){}. Load average before: {}; after: {}.\n",
            self.options.rounds,
            if self.options.small { ", small shapes only" } else { "" },
            field("load_before"),
            field("load_after"),
        );
        let _ = writeln!(out, "| Suite | Scenario | Metric | Median [min, max] | n |");
        let _ = writeln!(out, "|---|---|---|---|---:|");
        // Rows in first-seen order.
        let mut keys: Vec<(&str, &str, &str, &str)> = Vec::new();
        for sample in &self.samples {
            let key = (sample.suite, sample.scenario.as_str(), sample.metric, sample.unit);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        for (suite, scenario, metric, unit) in keys {
            let values: Vec<f64> = self
                .samples
                .iter()
                .filter(|s| s.suite == suite && s.scenario == scenario && s.metric == metric)
                .map(|s| s.value)
                .collect();
            if let Some(stats) = stats(values) {
                let _ = writeln!(out, "| {suite} | {scenario} | {metric} | {} | {} |", cell(&stats, unit), stats.n);
            }
        }
        for note in &self.notes {
            let _ = writeln!(out, "\n- {note}");
        }
        let _ = writeln!(
            out,
            "\nRaw samples: `results.jsonl`; probe stderr and runner output: `raw/`; machine details: `environment.json`."
        );
        out
    }
}

/// `discovery_probe sample` prints `key=value` pairs.
fn discovery_output(stdout: &str) -> Value {
    let fields: serde_json::Map<String, Value> = stdout
        .split_whitespace()
        .filter_map(|kv| kv.split_once('='))
        .map(|(key, value)| (key.to_string(), Value::String(value.to_string())))
        .collect();
    Value::Object(fields)
}

struct Stats {
    median: f64,
    min: f64,
    max: f64,
    n: usize,
}

fn stats(mut values: Vec<f64>) -> Option<Stats> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    let median = if n % 2 == 1 { values[n / 2] } else { (values[n / 2 - 1] + values[n / 2]) / 2.0 };
    Some(Stats { median, min: values[0], max: values[n - 1], n })
}

fn format_value(value: f64, unit: &str) -> String {
    match unit {
        "s" if value < 1.0 => format!("{:.1} ms", value * 1e3),
        "s" => format!("{value:.3} s"),
        other => format!("{value:.1} {other}"),
    }
}

fn cell(stats: &Stats, unit: &str) -> String {
    if stats.n == 1 {
        return format_value(stats.median, unit);
    }
    format!(
        "{} [{}, {}]",
        format_value(stats.median, unit),
        format_value(stats.min, unit),
        format_value(stats.max, unit)
    )
}

fn label(row: &Value) -> Result<&'static str> {
    match row["label"].as_str() {
        Some("rust") => Ok("rust"),
        other => Err(format!("unexpected runner label {other:?}")),
    }
}

fn number(value: &Value, key: &str) -> Result<f64> {
    value[key].as_f64().ok_or_else(|| format!("probe output has no numeric {key}: {value}"))
}

fn read_jsonl(path: &Path) -> Result<Vec<Value>> {
    let text = fs::read_to_string(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).map_err(|error| format!("{}: {error}", path.display())))
        .collect()
}

/// Copies the runner's `results.jsonl` and `*.stderr` (not the probe databases) into `raw`.
fn keep_runner_output(work: &Path, raw: &Path) -> Result<()> {
    let entries = fs::read_dir(work).map_err(|error| format!("could not read {}: {error}", work.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let keep = path.extension().is_some_and(|ext| ext == "stderr" || ext == "jsonl");
        if keep && path.is_file() {
            fs::copy(&path, raw.join(entry.file_name()))
                .map_err(|error| format!("could not copy {}: {error}", path.display()))?;
        }
    }
    Ok(())
}

// --- Environment -------------------------------------------------------------------------------

fn command_output(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "?".into())
}

fn load_average() -> String {
    command_output("sysctl", &["-n", "vm.loadavg"]).trim_matches(|c| c == '{' || c == '}' || c == ' ').to_string()
}

fn environment(root: &Path) -> Result<Value> {
    let memory: f64 = command_output("sysctl", &["-n", "hw.memsize"]).parse().unwrap_or(0.0);
    let root = root.to_string_lossy().to_string();
    Ok(json!({
        "date": command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"]),
        "cpu": command_output("sysctl", &["-n", "machdep.cpu.brand_string"]),
        "model": command_output("sysctl", &["-n", "hw.model"]),
        "cores": command_output("sysctl", &["-n", "hw.ncpu"]).parse::<u32>().unwrap_or(0),
        "memory_gib": (memory / 1_073_741_824.0).round(),
        "macos": command_output("sw_vers", &["-productVersion"]),
        "commit": command_output("git", &["-C", &root, "rev-parse", "--short", "HEAD"]),
        "dirty": !command_output("git", &["-C", &root, "status", "--porcelain"]).is_empty(),
        "rustc": command_output("rustc", &["--version"]),
        "load_before": load_average(),
    }))
}

/// A private fixture directory under `target/xtask`, removed on drop.
struct WorkDir(PathBuf);

impl WorkDir {
    fn new() -> Result<Self> {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_nanos());
        let base = fs::canonicalize(target_dir())
            .map_err(|error| format!("could not resolve the target directory: {error}"))?
            .join("xtask");
        let path = base.join(format!("bench-work.{}.{nanos}", std::process::id()));
        fs::create_dir_all(&path).map_err(|error| format!("could not create {}: {error}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_even_and_odd() {
        assert_eq!(stats(vec![3.0, 1.0, 2.0]).unwrap().median, 2.0);
        assert_eq!(stats(vec![4.0, 1.0, 2.0, 3.0]).unwrap().median, 2.5);
        assert!(stats(Vec::new()).is_none());
    }
}
