//! `cargo xtask bench-compare`: interleaved Rust vs. Swift v0.9.15 probe comparisons.
//!
//! Builds the `orbit-bench` probes (release profile), then runs every suite in fresh processes,
//! alternating which implementation goes first each round:
//!
//! - `indexing`: `indexing_probe` vs. Swift `IndexingProbe`, through the existing
//!   `scripts/benchmarks/run-durable-indexing.py` runner (and its summarizer).
//! - `marking`: `marking_probe` vs. Swift `MarkingProbe` (3 timed passes per process).
//! - `pipeline`: `pipeline_probe` vs. Swift `PipelineProbe`, through
//!   `scripts/benchmarks/run-indexing-pipeline.py`.
//! - `discovery`: `discovery_probe sample` vs. Swift `DiscoveryProbe producer`
//!   (`Benchmarks/Discovery`, built by `scripts/benchmarks/build-discovery.sh`) on the same
//!   generated flat and mixed fixtures, 1/4/8 workers (4 with `--small`), with CPU and RSS.
//! - `e2e`: `e2e_probe` (Rust only); the documented Swift reference is a different tree.
//!
//! Writes `results.jsonl` (one line per sample, with the probe's raw output), `summary.md`
//! (median/min/max and speedup) and per-run stderr under `raw/`. Generated fixtures live in a
//! private directory under `target/xtask` that is deleted afterwards. Reports, not timing gates
//! (see `docs/rust-port/README.md`, "Performance").

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::{Args, Result, exec, target_dir, workspace_root};

const USAGE: &str = "Usage: cargo xtask bench-compare --output NEW-DIR [--rounds N] [--swift-dir DIR] \
                     [--suites indexing,marking,pipeline,discovery,e2e] [--small]";
const SUITES: [&str; 5] = ["indexing", "marking", "pipeline", "discovery", "e2e"];

#[derive(Debug)]
struct Options {
    rounds: u32,
    swift_dir: PathBuf,
    output: PathBuf,
    suites: Vec<String>,
    small: bool,
}

fn parse(args: &[OsString]) -> Result<Options> {
    let (mut rounds, mut swift_dir, mut output, mut suites, mut small) = (3, None, None, None, false);
    let mut args = Args::new(args);
    while let Some(arg) = args.next() {
        match arg? {
            "--rounds" => {
                let value = args.value("--rounds")?;
                let parsed = value.to_str().and_then(|value| value.parse::<u32>().ok()).filter(|&value| value > 0);
                rounds = parsed.ok_or_else(|| format!("--rounds needs a positive integer, got {value:?}"))?;
            }
            "--swift-dir" => swift_dir = Some(PathBuf::from(args.value("--swift-dir")?)),
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
    let swift_dir = match swift_dir {
        Some(dir) => dir,
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set; pass --swift-dir")?).join("orbit-bench"),
    };
    Ok(Options {
        rounds,
        swift_dir,
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

/// A documented Swift number for a Rust-only suite.
struct Reference {
    suite: &'static str,
    scenario: String,
    metric: &'static str,
    value: f64,
    source: &'static str,
}

struct Run {
    options: Options,
    output: PathBuf,
    work: WorkDir,
    samples: Vec<Sample>,
    references: Vec<Reference>,
    notes: Vec<String>,
    jsonl: fs::File,
}

pub fn run(args: &[OsString]) -> Result<()> {
    let options = parse(args)?;
    let output = std::path::absolute(&options.output).map_err(|error| format!("invalid --output: {error}"))?;
    if fs::symlink_metadata(&output).is_ok() {
        return Err(format!("{} already exists; bench-compare writes into a new directory", output.display()));
    }
    let swift = swift_probes(&options.swift_dir, &options.suites)?;

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
    let mut run = Run {
        work: WorkDir::new()?,
        output: output.clone(),
        samples: Vec::new(),
        references: Vec::new(),
        notes: Vec::new(),
        jsonl,
        options,
    };
    let mut environment = environment(&root, swift.as_ref())?;
    eprintln!("bench-compare: load average before: {}", environment["load_before"].as_str().unwrap_or("?"));

    let suites = run.options.suites.clone();
    let swift_path = |kind: &str| swift.as_ref().expect("loaded for the selected suites").path(kind);
    let mut loads = Vec::new();
    for suite in &suites {
        let before = load_average();
        eprintln!("bench-compare: suite {suite} (load {before})");
        match suite.as_str() {
            "indexing" => run.indexing(&rust_dir.join("indexing_probe"), swift_path("indexing"))?,
            "marking" => run.marking(&rust_dir.join("marking_probe"), swift_path("marking"))?,
            "pipeline" => run.pipeline(&rust_dir.join("pipeline_probe"), swift_path("pipeline"))?,
            "discovery" => run.discovery(&rust_dir.join("discovery_probe"), swift_path("discovery"))?,
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

/// Interleaving: Rust first in odd rounds, Swift first in even rounds.
fn ordered<'a>(round: u32, rust: &'a Path, swift: &'a Path) -> [(&'static str, &'a Path); 2] {
    if round % 2 == 1 { [("rust", rust), ("swift", swift)] } else { [("swift", swift), ("rust", rust)] }
}

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

    /// The existing durable-indexing runner, interleaved, with its databases in the work dir.
    fn indexing(&mut self, rust: &Path, swift: &Path) -> Result<()> {
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
                .arg(format!("--binary=swift={}", swift.display()))
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

    fn marking(&mut self, rust: &Path, swift: &Path) -> Result<()> {
        let counts: &[u32] = if self.options.small { &[50_000] } else { &[50_000, 100_000] };
        for round in 1..=self.options.rounds {
            for &count in counts {
                for (label, probe) in ordered(round, rust, swift) {
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

    /// The existing pipeline runner (its fixture and caches live in its own `/private/tmp` dir).
    fn pipeline(&mut self, rust: &Path, swift: &Path) -> Result<()> {
        let shapes: &[u32] = if self.options.small { &[1000] } else { &[1000, 2000] };
        for &directories in shapes {
            let scenario = format!("{directories} folders x 10 files");
            let raw = self.raw_dir("pipeline")?.join(format!("{directories}-folders"));
            exec(
                Command::new("python3")
                    .arg(workspace_root().join("scripts/benchmarks/run-indexing-pipeline.py"))
                    .arg(format!("--binary=rust={}", rust.display()))
                    .arg(format!("--binary=swift={}", swift.display()))
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

    /// `discovery_probe sample` vs. Swift `DiscoveryProbe producer` on the same generated fixtures
    /// (flat, mixed), 1/4/8 workers, one fresh process per sample, order alternating per round.
    fn discovery(&mut self, rust: &Path, swift: &Path) -> Result<()> {
        let dir = self.work.0.join("discovery");
        self.probe("discovery", "fixtures", Command::new(rust).arg("fixtures").arg("--dir").arg(&dir))?;
        let fixtures = ["flat", "mixed"];
        let workers: &[u32] = if self.options.small { &[4] } else { &[1, 4, 8] };
        let mut expected = BTreeMap::new();
        for fixture in fixtures {
            expected.insert(fixture, discovery_manifest(&dir, fixture)?);
        }
        let command = |label: &str, fixture: &str, workers: u32| -> Command {
            let workers = workers.to_string();
            if label == "rust" {
                let mut command = Command::new(rust);
                command.arg("sample").arg("--dir").arg(&dir).args(["--fixture", fixture, "--workers", &workers]);
                command
            } else {
                let (records, files) = expected[fixture];
                let mut command = Command::new(swift);
                command.arg("producer").arg(dir.join(fixture)).args([workers, records.to_string(), files.to_string()]);
                command
            }
        };
        // Unmeasured warm-up of both implementations, as in `discovery_probe bench`.
        for fixture in fixtures {
            for label in ["rust", "swift"] {
                self.probe("discovery", &format!("warmup-{fixture}-{label}"), &mut command(label, fixture, 4))?;
            }
        }
        for round in 1..=self.options.rounds {
            for fixture in fixtures {
                for index in 0..workers.len() {
                    // Rotate the worker order too, so no count always runs first.
                    let count = workers[(index + round as usize) % workers.len()];
                    for (label, _) in ordered(round, rust, swift) {
                        let name = format!("r{round}-{fixture}-w{count}-{label}");
                        let load = load_average();
                        let stdout = self.probe("discovery", &name, &mut command(label, fixture, count))?;
                        let mut raw = discovery_output(label, &stdout).map_err(|error| format!("{name}: {error}"))?;
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
            "discovery: discovery into the live inventory only (no writer). Swift runs `AsyncListingProducer` with a \
             `LiveMappingInventory` (Benchmarks/Discovery/DiscoveryProbe.swift); its four-worker clamp is raised in the \
             benchmark copy so 8 workers mean 8. CPU rows are process user/system time over the same span. The \
             unpaired 2026-10-28 Swift numbers in docs/benchmarks/parallel-discovery.json are not comparable \
             (see docs/benchmarks/rust-port/discovery-scaling.md)."
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
        if !self.options.small {
            // docs/benchmarks/rust-port/session-wave-c.md: Swift on a real Documents folder.
            for (metric, value) in [("total to publication", 9.74), ("discovery", 1.46), ("first full view", 1.68)] {
                self.references.push(Reference {
                    suite: "e2e",
                    scenario: "180k files".into(),
                    metric,
                    value,
                    source: "docs/benchmarks/rust-port/session-wave-c.md (real Documents folder, 179k files)",
                });
            }
        }
        self.notes.push(
            "e2e: Rust only on a generated tree; the documented Swift reference was a real Documents folder, so it \
             is indicative only and no speedup is computed."
                .into(),
        );
        Ok(())
    }

    // --- Summary -------------------------------------------------------------------------------

    fn summary(&self, environment: &Value) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Rust vs Swift v0.9.15: `cargo xtask bench-compare`\n");
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
            "{} fresh-process round(s), implementations interleaved (order alternates per round){}. \
             Load average before: {}; after: {}.\n",
            self.options.rounds,
            if self.options.small { ", small shapes only" } else { "" },
            field("load_before"),
            field("load_after"),
        );
        let _ = writeln!(
            out,
            "| Suite | Scenario | Metric | Rust median [min, max] | Swift median [min, max] | Speedup | n (Rust/Swift) |"
        );
        let _ = writeln!(out, "|---|---|---|---|---|---:|---|");
        // Rows in first-seen order.
        let mut keys: Vec<(&str, &str, &str, &str)> = Vec::new();
        for sample in &self.samples {
            let key = (sample.suite, sample.scenario.as_str(), sample.metric, sample.unit);
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        for (suite, scenario, metric, unit) in keys {
            let values = |label: &str| -> Vec<f64> {
                self.samples
                    .iter()
                    .filter(|s| s.suite == suite && s.scenario == scenario && s.metric == metric && s.label == label)
                    .map(|s| s.value)
                    .collect()
            };
            let (rust, swift) = (stats(values("rust")), stats(values("swift")));
            let reference =
                self.references.iter().find(|r| r.suite == suite && r.scenario == scenario && r.metric == metric);
            let swift_cell = match (&swift, reference) {
                (Some(s), _) => cell(s, unit),
                (None, Some(r)) => format!("{} (documented, not paired)", format_value(r.value, unit)),
                (None, None) => "–".into(),
            };
            // Speedup only for paired time metrics; lower is better.
            let speedup = match (&rust, &swift) {
                (Some(r), Some(s)) if unit == "s" && r.median > 0.0 => format!("{:.2}×", s.median / r.median),
                (Some(r), Some(s)) if r.median > 0.0 => format!("Swift/Rust {:.2}", s.median / r.median),
                _ => "–".into(),
            };
            let n = |s: &Option<Stats>| s.as_ref().map_or(0, |s| s.n);
            let _ = writeln!(
                out,
                "| {suite} | {scenario} | {metric} | {} | {swift_cell} | {speedup} | {}/{} |",
                rust.as_ref().map_or("–".into(), |s| cell(s, unit)),
                n(&rust),
                n(&swift),
            );
        }
        let _ =
            writeln!(out, "\nSpeedup is the Swift median divided by the Rust median (higher means Rust is faster).");
        for note in &self.notes {
            let _ = writeln!(out, "\n- {note}");
        }
        let sources: Vec<&str> = {
            let mut s: Vec<&str> = self.references.iter().map(|r| r.source).collect();
            s.dedup();
            s
        };
        for source in sources {
            let _ = writeln!(out, "- Documented Swift reference: {source}.");
        }
        let _ = writeln!(
            out,
            "\nRaw samples: `results.jsonl`; probe stderr and runner output: `raw/`; machine details: `environment.json`."
        );
        out
    }
}

/// `records=N files=M` written by `discovery_probe fixtures` next to each fixture.
fn discovery_manifest(dir: &Path, fixture: &str) -> Result<(u64, u64)> {
    let path = dir.join(format!("{fixture}.manifest"));
    let text = fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    let value = |key: &str| -> Result<u64> {
        text.split_whitespace()
            .find_map(|kv| kv.strip_prefix(key))
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| format!("{}: no {key}", path.display()))
    };
    Ok((value("records=")?, value("files=")?))
}

/// The Rust probe prints `key=value` pairs, the Swift probe one JSON object.
fn discovery_output(label: &str, stdout: &str) -> Result<Value> {
    if label == "swift" {
        return serde_json::from_str(stdout.trim()).map_err(|error| error.to_string());
    }
    let fields: serde_json::Map<String, Value> = stdout
        .split_whitespace()
        .filter_map(|kv| kv.split_once('='))
        .map(|(key, value)| (key.to_string(), Value::String(value.to_string())))
        .collect();
    Ok(Value::Object(fields))
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
        Some("swift") => Ok("swift"),
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

// --- Swift probes and environment ---------------------------------------------------------------

struct SwiftProbes {
    paths: BTreeMap<&'static str, PathBuf>,
    manifests: Value,
}

impl SwiftProbes {
    fn path(&self, kind: &str) -> &Path {
        self.paths.get(kind).expect("loaded for every selected suite")
    }
}

/// `DIR/swift-v0.9.15-<suite>/<Probe>` for every selected suite with a Swift probe, as built by
/// `scripts/benchmarks/build-durable-indexing.sh` (indexing, marking, pipeline) and
/// `scripts/benchmarks/build-discovery.sh` (discovery); each binary must match its manifest hash.
/// `None` when no selected suite has one.
fn swift_probes(dir: &Path, suites: &[String]) -> Result<Option<SwiftProbes>> {
    const PROBES: [(&str, &str, &str); 4] = [
        ("indexing", "IndexingProbe", "build-durable-indexing.sh <v0.9.15 checkout> {} grouped indexing"),
        ("marking", "MarkingProbe", "build-durable-indexing.sh <v0.9.15 checkout> {} grouped marking"),
        ("pipeline", "PipelineProbe", "build-durable-indexing.sh <v0.9.15 checkout> {} grouped pipeline"),
        ("discovery", "DiscoveryProbe", "build-discovery.sh <v0.9.15 checkout> {}"),
    ];
    let mut manifests = serde_json::Map::new();
    let mut paths = BTreeMap::new();
    for (kind, binary, build) in PROBES {
        if !suites.iter().any(|suite| suite == kind) {
            continue;
        }
        let folder = dir.join(format!("swift-v0.9.15-{kind}"));
        let path = folder.join(binary);
        if !path.is_file() {
            return Err(format!(
                "Swift probe {} is missing. Build it from the v0.9.15 tag with `scripts/benchmarks/{}` or pass \
                 --swift-dir.",
                path.display(),
                build.replace("{}", &folder.display().to_string()),
            ));
        }
        let actual = sha256(&path)?;
        let manifest: Value = fs::read_to_string(folder.join("manifest.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null);
        if let Some(expected) = manifest["binary_sha256"].as_str()
            && expected != actual
        {
            return Err(format!("{} does not match its manifest.json hash", path.display()));
        }
        manifests.insert(
            kind.into(),
            json!({"path": path, "binary_sha256": actual, "swift": manifest["swift"], "patches": manifest["patches"]}),
        );
        paths.insert(kind, path);
    }
    Ok((!paths.is_empty()).then_some(SwiftProbes { paths, manifests: Value::Object(manifests) }))
}

fn sha256(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    Ok(Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02x}")).collect())
}

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

fn environment(root: &Path, swift: Option<&SwiftProbes>) -> Result<Value> {
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
        "swift_probes": swift.map_or(Value::Null, |swift| swift.manifests.clone()),
    }))
}

/// A private fixture directory under `target/xtask`, removed on drop. Not under `/tmp` or
/// `$TMPDIR`: both resolve through `/private`, which Foundation strips from the Swift probes'
/// paths ("missing node parent").
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
    fn interleaving_alternates() {
        let (rust, swift) = (Path::new("r"), Path::new("s"));
        assert_eq!(ordered(1, rust, swift)[0].0, "rust");
        assert_eq!(ordered(2, rust, swift)[0].0, "swift");
    }

    #[test]
    fn median_of_even_and_odd() {
        assert_eq!(stats(vec![3.0, 1.0, 2.0]).unwrap().median, 2.0);
        assert_eq!(stats(vec![4.0, 1.0, 2.0, 3.0]).unwrap().median, 2.5);
        assert!(stats(Vec::new()).is_none());
    }
}
