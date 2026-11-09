//! The small JSON settings file in `AppDirs::settings` (Places, last location, one-shot flags).
//! Written atomically (temporary file + rename) off the UI thread; the newest write wins.
//!
//! Paths are raw bytes: valid UTF-8 is stored as a string, anything else as `{"hex": "…"}`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use gpui::BackgroundExecutor;
use orbit_app_state::event::Persist;
use serde_json::{Map, Value, json};

pub const SETTINGS_FILE: &str = "settings.json";
/// Fixture runs keep their own file so fixture paths never leak into real settings.
pub const FIXTURE_SETTINGS_FILE: &str = "settings-fixture.json";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub places: Vec<Vec<u8>>,
    pub last_location: Option<Vec<u8>>,
    /// The Swift app's settings were imported (or there were none).
    pub legacy_imported: bool,
    /// "Clear Cache…" was confirmed; the next launch removes the cache before anything opens it.
    pub clear_cache_on_launch: bool,
}

impl Settings {
    pub fn apply(&mut self, persist: Persist) {
        match persist {
            Persist::Places(places) => self.places = places,
            Persist::LastLocation(path) => self.last_location = Some(path),
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "version": 1,
            "places": self.places.iter().map(|p| encode_path(p)).collect::<Vec<_>>(),
            "lastLocation": self.last_location.as_deref().map(encode_path),
            "legacyImported": self.legacy_imported,
            "clearCacheOnLaunch": self.clear_cache_on_launch,
        })
    }

    /// Unknown or malformed fields fall back to defaults.
    pub fn from_json(value: &Value) -> Self {
        let empty = Map::new();
        let object = value.as_object().unwrap_or(&empty);
        Self {
            places: object
                .get("places")
                .and_then(Value::as_array)
                .map(|places| places.iter().filter_map(decode_path).collect())
                .unwrap_or_default(),
            last_location: object.get("lastLocation").and_then(decode_path),
            legacy_imported: object.get("legacyImported").and_then(Value::as_bool).unwrap_or(false),
            clear_cache_on_launch: object.get("clearCacheOnLaunch").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

fn encode_path(path: &[u8]) -> Value {
    match std::str::from_utf8(path) {
        Ok(text) => Value::String(text.to_owned()),
        Err(_) => json!({ "hex": path.iter().map(|b| format!("{b:02x}")).collect::<String>() }),
    }
}

fn decode_path(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.as_bytes().to_vec()),
        Value::Object(object) => {
            let hex = object.get("hex")?.as_str()?;
            if hex.len() % 2 != 0 {
                return None;
            }
            (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect()
        }
        _ => None,
    }
}

/// Settings plus where they live. `path == None` keeps them in memory (tests).
#[derive(Debug)]
pub struct SettingsStore {
    path: Option<PathBuf>,
    current: Settings,
    existed: bool,
    generation: u64,
    /// Generation of the newest completed write; serializes background writers.
    written: Arc<Mutex<u64>>,
}

impl SettingsStore {
    pub fn in_memory(settings: Settings) -> Self {
        Self { path: None, current: settings, existed: true, generation: 0, written: Arc::default() }
    }

    /// Reads `path`; a missing or unreadable file yields defaults.
    pub fn load(path: PathBuf) -> Self {
        let (current, existed) = match std::fs::read(&path) {
            Ok(bytes) => {
                (serde_json::from_slice::<Value>(&bytes).map(|v| Settings::from_json(&v)).unwrap_or_default(), true)
            }
            Err(_) => (Settings::default(), false),
        };
        Self { path: Some(path), current, existed, generation: 0, written: Arc::default() }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// No settings file existed at load (first launch of this app).
    pub fn is_first_launch(&self) -> bool {
        !self.existed
    }

    pub fn settings(&self) -> &Settings {
        &self.current
    }

    /// Changes the settings; returns whether anything changed (the caller then saves).
    pub fn update(&mut self, change: impl FnOnce(&mut Settings)) -> bool {
        let before = self.current.clone();
        change(&mut self.current);
        before != self.current
    }

    /// Writes the current settings on `executor`. Older pending writes are skipped.
    pub fn save(&mut self, executor: &BackgroundExecutor) {
        let Some(path) = self.path.clone() else { return };
        self.generation += 1;
        let generation = self.generation;
        let bytes = self.bytes();
        let written = Arc::clone(&self.written);
        executor
            .spawn(async move {
                let mut newest = written.lock().unwrap_or_else(|e| e.into_inner());
                if *newest >= generation {
                    return;
                }
                if let Err(error) = write_atomic(&path, &bytes) {
                    eprintln!("orbit-disk: couldn't save settings to {}: {error}", path.display());
                }
                *newest = generation;
            })
            .detach();
    }

    /// Writes synchronously (before quitting or relaunching).
    pub fn save_now(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else { return Ok(()) };
        self.generation += 1;
        let mut newest = self.written.lock().unwrap_or_else(|e| e.into_inner());
        write_atomic(&path, &self.bytes())?;
        *newest = self.generation;
        Ok(())
    }

    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(&self.current.to_json()).unwrap_or_default()
    }
}

/// Writes `bytes` to a sibling temporary file, syncs it and renames it over `path`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}.tmp", std::process::id()));
    let temporary = parent.join(name);
    let result = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
