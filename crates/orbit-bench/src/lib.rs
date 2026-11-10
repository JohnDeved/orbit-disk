//! Benchmarks: criterion micro-benches and probe binaries.
//!
//! Shared helpers for the durable-indexing probes (`indexing_probe`, `marking_probe`), whose
//! JSON output is what `scripts/benchmarks/*-durable-indexing.py` read (the format of the
//! original Swift probes).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

/// Accumulated seconds and calls per phase.
#[derive(Debug, Default)]
pub struct Timings {
    pub seconds: BTreeMap<String, f64>,
    pub calls: BTreeMap<String, u64>,
}

impl Timings {
    pub fn measure<T>(&mut self, label: &str, body: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let value = body();
        *self.seconds.entry(label.to_string()).or_default() += start.elapsed().as_secs_f64();
        *self.calls.entry(label.to_string()).or_default() += 1;
        value
    }
}

/// Peak RSS (MiB) and user/system CPU seconds of this process.
pub fn rss_and_cpu() -> serde_json::Value {
    // SAFETY: zeroed rusage is valid POD; getrusage writes it.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid out-pointer.
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    #[cfg(target_os = "macos")]
    let rss = usage.ru_maxrss as f64 / 1_048_576.0;
    #[cfg(not(target_os = "macos"))]
    let rss = usage.ru_maxrss as f64 / 1024.0;
    serde_json::json!({
        "peak_rss_mib": rss,
        "user_seconds": seconds(usage.ru_utime),
        "system_seconds": seconds(usage.ru_stime),
    })
}

pub fn file_bytes(path: &Path) -> i64 {
    std::fs::symlink_metadata(path).map_or(0, |m| m.len() as i64)
}

/// WAL file size plus the read-only WAL-index header fields (frames, backfill, page size),
/// sampled between synchronous writer calls like the Swift probe.
pub fn wal_state(database: &Path) -> BTreeMap<String, i64> {
    let mut result = BTreeMap::new();
    let wal = format!("{}-wal", database.display());
    result.insert("file_bytes".to_string(), file_bytes(Path::new(&wal)));
    let shm = format!("{}-shm", database.display());
    let Ok(bytes) = std::fs::read(&shm) else { return result };
    if bytes.len() < 100 || bytes[..48] != bytes[48..96] || bytes[12] != 1 {
        return result;
    }
    let u32_at = |offset: usize| u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap()) as i64;
    result.insert("index_version".to_string(), u32_at(0));
    result.insert("frames".to_string(), u32_at(16));
    result.insert("backfilled_frames".to_string(), u32_at(96));
    result.insert("page_size".to_string(), u16::from_ne_bytes([bytes[14], bytes[15]]) as i64);
    result
}

/// Creates a new, private output directory (the probes refuse to reuse one).
pub fn create_output(directory: &Path) -> Result<(), String> {
    if directory.exists() {
        return Err(format!("{} already exists", directory.display()));
    }
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(directory).map_err(|e| e.to_string())
}
