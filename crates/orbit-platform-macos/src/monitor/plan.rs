//! What one FSEvents stream watches and how its raw event paths map back into the mapped root.
//!
//! A root is watched either *device-relative* (the usual case: the root lives below a mount, and
//! FSEvents reports paths relative to that device) or *host-wide* (the root is `/`, or it cannot be
//! expressed relative to its mount). Event paths arrive in the filesystem's physical spelling
//! (`/private/var/...`, `/System/Volumes/Data/Users/...`); normalization rewrites them into the
//! caller's spelling of the root and drops anything that escapes it through `..` or symlinks.

use std::collections::HashMap;
use std::path::Path;

use orbit_platform::{ChangeCheckpoint, PlatformError, Result};

use crate::ffi;
use crate::firmlinks::{self, DATA_VOLUME};
use crate::paths::{
    self, contains, has_dot_dot, matches_through_existing_ancestor, parent, realpath, standardize, trim_slashes,
    under_mount,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub mount_path: Vec<u8>,
    pub volume_uuid: Option<String>,
    pub events_uuid: Option<String>,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
pub struct EventPlan {
    /// The caller's (standardized) spelling of the root; batches use it.
    pub root_path: Vec<u8>,
    /// `realpath` of the root.
    pub canonical_root: Vec<u8>,
    /// The root addressed through its mount (`/System/Volumes/Data/...`) in device mode.
    pub physical_root: Vec<u8>,
    pub mount_path: Vec<u8>,
    /// Path relative to the device root; `None` for a host-wide stream.
    pub relative_path: Option<Vec<u8>>,
    /// Other volumes below a whole-Mac root whose events are ignored.
    pub untracked_mounts: Vec<Vec<u8>>,
    pub device: libc::dev_t,
    pub root_file_id: u64,
    pub supports_replay: bool,
    pub checkpoint: ChangeCheckpoint,
    /// The identity earlier builds saved for this whole-Mac root, which also listed every other
    /// mounted volume; `None` when it is the same as the current identity.
    pub legacy_identity: Option<String>,
}

fn invalid_root(root: &Path) -> PlatformError {
    PlatformError::os(root, libc::EINVAL)
}

impl EventPlan {
    pub fn inspect(root: &Path) -> Result<Self> {
        let raw = paths::bytes(root);
        if raw.first() != Some(&b'/') || raw.contains(&0) {
            return Err(invalid_root(root));
        }
        let root_path = standardize(raw);
        let canonical_root = realpath(&root_path).map_err(|code| PlatformError::os(root, code))?;
        let metadata = paths::stat(&canonical_root).filter(paths::is_dir).ok_or_else(|| invalid_root(root))?;
        let filesystem = paths::statfs_path(&canonical_root).ok_or_else(|| PlatformError::last_os(root))?;
        let mount = paths::mount_on_name(&filesystem);
        let mut physical_root = canonical_root.clone();
        let mut relative_path = None;
        if root_path != b"/" {
            if contains(&canonical_root, &mount) {
                let skip = if mount == b"/" { 1 } else { mount.len() };
                relative_path = Some(trim_slashes(&canonical_root[skip.min(canonical_root.len())..]).to_vec());
            } else {
                let candidate = standardize(&under_mount(&mount, &canonical_root));
                if paths::same_file(&candidate, &canonical_root) {
                    physical_root = candidate;
                    relative_path = Some(trim_slashes(&canonical_root).to_vec());
                }
            }
        }
        let host_id = if relative_path.is_none() { crate::system::machine_identity() } else { None };
        let volumes = if relative_path.is_some() {
            vec![volume(&filesystem)]
        } else {
            let mut volumes: Vec<Volume> = paths::mounted_filesystems()
                .iter()
                .filter(|fs| fs.f_flags & libc::MNT_LOCAL as u32 != 0)
                .map(volume)
                .filter(|v| v.volume_uuid.is_some() || v.events_uuid.is_some())
                .collect();
            volumes.sort_by(|a, b| a.mount_path.cmp(&b.mount_path));
            volumes.dedup_by(|a, b| a.mount_path == b.mount_path);
            volumes
        };
        let mut untracked_mounts: Vec<Vec<u8>> = if relative_path.is_none() && root_path == b"/" {
            volumes.iter().map(|v| v.mount_path.clone()).filter(|m| m != b"/").collect()
        } else {
            Vec::new()
        };
        let root_identity = orbit_platform::posix::stat_identity(&metadata);
        if root_path == b"/"
            && paths::directory_identity(b"/") == Some(root_identity)
            && firmlinks::capture(firmlinks::system_links()).is_some()
        {
            untracked_mounts.retain(|m| m != DATA_VOLUME.as_bytes());
        }
        // Only the volumes the map covers belong to its history: an external disk, disk image or
        // simulator volume mounted or ejected meanwhile neither changes the map nor its events.
        let tracked: Vec<Volume> =
            volumes.iter().filter(|v| !untracked_mounts.contains(&v.mount_path)).cloned().collect();
        // Boot helper volumes have no journals; the verified Data volume is part of whole-Mac mapping.
        let supports_replay = !tracked.is_empty()
            && tracked.iter().all(|v| v.volume_uuid.is_some())
            && if relative_path.is_some() {
                tracked[0].events_uuid.is_some()
            } else {
                host_id.is_some() && tracked.iter().any(|v| v.events_uuid.is_some())
            };
        let fields = IdentityFields {
            root_path: &root_path,
            canonical_root: &canonical_root,
            root_file_id: metadata.st_ino,
            device_mode: relative_path.is_some(),
            host_id: host_id.as_deref(),
            volumes: &tracked,
            boot_traversal: root_path == b"/",
        };
        let identity = identity_json(&fields);
        let legacy_identity =
            (tracked.len() != volumes.len()).then(|| identity_json(&IdentityFields { volumes: &volumes, ..fields }));
        let event_id = if relative_path.is_some() {
            // This FSEvents API takes Unix-epoch seconds despite its CFAbsoluteTime type.
            let now =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
            // SAFETY: plain FSEvents query.
            unsafe { ffi::FSEventsGetLastEventIdForDeviceBeforeTime(metadata.st_dev, now) }
        } else {
            // SAFETY: plain FSEvents query.
            unsafe { ffi::FSEventsGetCurrentEventId() }
        };
        Ok(Self {
            checkpoint: ChangeCheckpoint { root: paths::to_path(&root_path), identity, event_id },
            root_path,
            canonical_root,
            physical_root,
            mount_path: mount,
            relative_path,
            untracked_mounts,
            device: metadata.st_dev,
            root_file_id: metadata.st_ino,
            supports_replay,
            legacy_identity,
        })
    }

    /// Whether a saved checkpoint `identity` belongs to this root's current change history:
    /// the current identity, or the one earlier builds saved while the same volumes were mounted.
    pub fn same_history(&self, identity: &str) -> bool {
        identity == self.checkpoint.identity || self.legacy_identity.as_deref() == Some(identity)
    }

    /// Whether a volume mounted or unmounted at `path` changes what this root's history covers:
    /// a mount at or above the root, or the whole-Mac map's own Data volume.
    pub fn mount_changes_history(&self, path: &[u8]) -> bool {
        contains(&self.root_path, path)
            || (self.relative_path.is_none()
                && self.root_path == b"/"
                && path == DATA_VOLUME.as_bytes()
                && !self.untracked_mounts.iter().any(|m| m == path))
    }

    /// The root is still the directory this plan was made for.
    pub fn root_identity_is_valid(&self) -> bool {
        paths::lstat(&self.canonical_root)
            .is_some_and(|info| info.st_dev == self.device && info.st_ino == self.root_file_id && paths::is_dir(&info))
    }

    fn native_path(&self, raw: &[u8]) -> Vec<u8> {
        under_mount(&self.mount_path, trim_slashes(raw))
    }

    /// Normalizes one raw event path through a per-batch parent cache.
    pub fn normalized_event_path(&self, raw: &[u8], cache: &mut EventPathCache) -> Option<Vec<u8>> {
        let separator = raw.iter().rposition(|&b| b == b'/');
        let usable = !raw.contains(&0) && !has_dot_dot(raw) && separator.is_some_and(|s| s + 1 != raw.len());
        let Some(separator) = separator.filter(|_| usable) else { return self.normalized_uncached(raw) };
        let parent_raw: &[u8] = if separator == 0 { b"/" } else { &raw[..separator] };
        let name = &raw[separator + 1..];
        let native_parent =
            if self.relative_path.is_some() { self.native_path(parent_raw) } else { parent_raw.to_vec() };
        let info = paths::lstat(&native_parent).filter(paths::is_dir);
        if let (Some(info), Some(entry)) = (info.as_ref(), cache.parents.get(parent_raw))
            && entry.native_parent == native_parent
            && entry.device == info.st_dev
            && entry.inode == info.st_ino
        {
            return Some(paths::child(&entry.parent, name));
        }
        cache.parents.remove(parent_raw);
        let tracked = info.is_some() && cache.track_ancestors(&native_parent);
        let normalized = self.normalized_uncached(raw);
        if tracked && !cache.revalidate_path(&native_parent) {
            cache.invalidated = true;
        }
        let normalized = normalized?;
        let mut suffix = Vec::with_capacity(name.len() + 1);
        suffix.push(b'/');
        suffix.extend_from_slice(name);
        if !tracked || cache.invalidated || !normalized.ends_with(&suffix) {
            return Some(normalized);
        }
        let normalized_parent = &normalized[..normalized.len() - suffix.len()];
        let info = info.expect("tracked implies a usable parent");
        cache.parents.insert(
            parent_raw.to_vec(),
            CacheEntry {
                parent: if normalized_parent.is_empty() { b"/".to_vec() } else { normalized_parent.to_vec() },
                native_parent,
                device: info.st_dev,
                inode: info.st_ino,
            },
        );
        Some(normalized)
    }

    pub fn normalized_uncached(&self, raw: &[u8]) -> Option<Vec<u8>> {
        if raw.contains(&0) || has_dot_dot(raw) {
            return None;
        }
        let path = if self.relative_path.is_some() {
            let path = standardize(&under_mount(&self.mount_path, raw));
            let native = self.native_path(raw);
            if contains(&path, &self.physical_root) {
                if path != self.physical_root {
                    let resolved_root = realpath(&self.physical_root).ok()?;
                    resolve_event_path(&native, &resolved_root)?;
                }
                rebase(&self.root_path, &path, &self.physical_root)
            } else if contains(&self.physical_root, &path) {
                return Some(self.root_path.clone());
            } else {
                let resolved_root = realpath(&self.physical_root).ok()?;
                let resolved = resolve_event_path(&native, &resolved_root)?;
                if !contains(&resolved, &resolved_root) {
                    return None;
                }
                rebase(&self.root_path, &resolved, &resolved_root)
            }
        } else {
            if raw.first() != Some(&b'/') {
                return None;
            }
            let mut path = standardize(raw);
            let data = DATA_VOLUME.as_bytes();
            if self.canonical_root == b"/" && path.len() > data.len() && contains(&path, data) {
                let alias = path[data.len()..].to_vec();
                if matches_through_existing_ancestor(&path, &alias) {
                    path = alias;
                }
            }
            if contains(&path, &self.canonical_root) {
                if path != self.canonical_root {
                    let resolved_root = realpath(&self.canonical_root).ok()?;
                    resolve_event_path(raw, &resolved_root)?;
                }
                rebase(&self.root_path, &path, &self.canonical_root)
            } else if contains(&self.canonical_root, &path) {
                return Some(self.root_path.clone());
            } else {
                let physical_root = realpath(&self.canonical_root).ok()?;
                let physical_event = resolve_event_path(raw, &physical_root)?;
                if !contains(&physical_event, &physical_root) {
                    return None;
                }
                rebase(&self.root_path, &physical_event, &physical_root)
            }
        };
        contains(&path, &self.root_path).then_some(path)
    }

    /// Raw spellings (as FSEvents reports them) of a native exclusion path.
    pub fn raw_exclusion_paths(&self, native: &[u8]) -> Vec<Vec<u8>> {
        if self.relative_path.is_none() {
            let mut out = vec![native.to_vec()];
            if self.canonical_root == b"/" && native.first() == Some(&b'/') && native != b"/" {
                let mut data_path = DATA_VOLUME.as_bytes().to_vec();
                data_path.extend_from_slice(native);
                if matches_through_existing_ancestor(&data_path, native) {
                    out.push(data_path);
                }
            }
            return out;
        }
        let suffix: Vec<u8> = if contains(native, &self.mount_path) {
            if self.mount_path == b"/" {
                native[1..].to_vec()
            } else {
                trim_slashes(&native[self.mount_path.len()..]).to_vec()
            }
        } else {
            if native.first() != Some(&b'/') {
                return Vec::new();
            }
            let mut candidate = self.mount_path.clone();
            candidate.extend_from_slice(native);
            if !matches_through_existing_ancestor(&candidate, native) {
                return Vec::new();
            }
            native[1..].to_vec()
        };
        if suffix.is_empty() {
            return vec![b"/".to_vec(), Vec::new()];
        }
        let mut absolute = vec![b'/'];
        absolute.extend_from_slice(&suffix);
        vec![suffix, absolute]
    }

    /// An event that could not be mapped may still concern the root itself (or an ancestor).
    pub fn event_may_affect_root(&self, raw: &[u8]) -> bool {
        if raw.contains(&0) || has_dot_dot(raw) {
            return true;
        }
        let path = if self.relative_path.is_some() {
            self.native_path(raw)
        } else {
            if raw.first() != Some(&b'/') {
                return true;
            }
            raw.to_vec()
        };
        let Ok(resolved_root) = realpath(&self.canonical_root) else { return true };
        [&self.canonical_root, &self.physical_root, &resolved_root]
            .iter()
            .any(|root| contains(&path, root) || contains(root, &path))
    }
}

/// `root` + the part of `path` below `prefix`.
fn rebase(root: &[u8], path: &[u8], prefix: &[u8]) -> Vec<u8> {
    let suffix = if prefix == b"/" { path } else { &path[prefix.len()..] };
    if root == b"/" {
        return if suffix.is_empty() { b"/".to_vec() } else { suffix.to_vec() };
    }
    let mut out = root.to_vec();
    out.extend_from_slice(suffix);
    out
}

/// Resolves the existing part of `raw` and re-appends the removed tail lexically, keeping the
/// final component unresolved (it may be a deleted item or a symlink itself). `None` when the
/// resolved part leaves `root`.
pub fn resolve_event_path(raw: &[u8], root: &[u8]) -> Option<Vec<u8>> {
    if raw.first() != Some(&b'/') {
        return None;
    }
    let components: Vec<&[u8]> = raw.split(|&b| b == b'/').filter(|c| !c.is_empty()).collect();
    let Some((last, prefix)) = components.split_last() else { return Some(b"/".to_vec()) };
    let mut prefix: Vec<&[u8]> = prefix.to_vec();
    let mut suffix: Vec<&[u8]> = vec![last];
    loop {
        let candidate = paths::join_components(&prefix);
        match realpath(&candidate) {
            Ok(resolved) => {
                if !contains(&resolved, root) {
                    return None;
                }
                let mut out = if resolved == b"/" { Vec::new() } else { resolved };
                for part in &suffix {
                    out.push(b'/');
                    out.extend_from_slice(part);
                }
                return Some(out);
            }
            Err(code) if code == libc::ENOENT || code == libc::ENOTDIR => {
                let last = prefix.pop()?;
                suffix.insert(0, last);
            }
            Err(_) => return None,
        }
    }
}

/// The physical spelling of an exclusion, even when it does not exist yet.
pub fn native_exclusion_path(path: &[u8]) -> Option<Vec<u8>> {
    if path == b"/" {
        return Some(b"/".to_vec());
    }
    realpath(path).ok().or_else(|| resolve_event_path(path, b"/"))
}

/// Byte-prefix exclusion test on a raw event path; refuses anything with `..` components.
pub fn raw_path_excluded(path: &[u8], exclusions: &[Vec<u8>]) -> bool {
    if path.contains(&0)
        || path.windows(4).any(|w| w == b"/../")
        || path.starts_with(b"../")
        || path == b".."
        || path.ends_with(b"/..")
    {
        return false;
    }
    exclusions.iter().any(|prefix| {
        let count = prefix.len();
        count <= path.len()
            && path.starts_with(prefix)
            && (count == 0 || count == path.len() || prefix == b"/" || path[count] == b'/')
    })
}

struct CacheEntry {
    parent: Vec<u8>,
    native_parent: Vec<u8>,
    device: libc::dev_t,
    inode: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct AncestorIdentity {
    device: libc::dev_t,
    inode: u64,
    mode: libc::mode_t,
}

fn ancestor_identity(path: &[u8]) -> Option<AncestorIdentity> {
    paths::lstat(path).map(|info| AncestorIdentity { device: info.st_dev, inode: info.st_ino, mode: info.st_mode })
}

/// Per-batch cache of normalized parents. Every ancestor that a cached mapping depends on is
/// recorded and re-checked at the end of the batch; a moved or replaced ancestor invalidates it.
#[derive(Default)]
pub struct EventPathCache {
    parents: HashMap<Vec<u8>, CacheEntry>,
    ancestors: HashMap<Vec<u8>, AncestorIdentity>,
    invalidated: bool,
}

fn prefixes(path: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
    let components: Vec<&[u8]> = path.split(|&b| b == b'/').filter(|c| !c.is_empty()).collect();
    (0..=components.len()).map(move |count| paths::join_components(&components[..count]))
}

impl EventPathCache {
    fn track_ancestors(&mut self, path: &[u8]) -> bool {
        let count = path.split(|&b| b == b'/').filter(|c| !c.is_empty()).count();
        if count > 128 || self.parents.len() >= 256 || self.ancestors.len() + count + 1 > 1024 {
            return false;
        }
        for current in prefixes(path) {
            let Some(identity) = ancestor_identity(&current) else { return false };
            if let Some(previous) = self.ancestors.get(&current)
                && *previous != identity
            {
                self.invalidated = true;
                return false;
            }
            self.ancestors.insert(current, identity);
        }
        true
    }

    fn revalidate_path(&self, path: &[u8]) -> bool {
        prefixes(path).all(|current| {
            self.ancestors.get(&current).is_some_and(|previous| ancestor_identity(&current) == Some(*previous))
        })
    }

    /// Every cached mapping still holds.
    pub fn revalidate(&self) -> bool {
        !self.invalidated && self.ancestors.iter().all(|(path, previous)| ancestor_identity(path) == Some(*previous))
    }
}

/// The outcome of one FSEvents callback after normalization.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PreparedBatch {
    /// Listings to repeat (see [`orbit_platform::ChangeBatch::directories`]).
    pub directories: std::collections::BTreeSet<Vec<u8>>,
    /// Directories to reconcile recursively.
    pub subtrees: std::collections::BTreeSet<Vec<u8>>,
    pub full_refresh: bool,
    pub identity_invalidated: bool,
    pub history_finished: bool,
    pub latest_event_id: u64,
}

pub struct RawEvent<'a> {
    pub path: &'a [u8],
    pub flags: u32,
    pub id: u64,
}

/// Maps raw events to directories to reconcile. Returns `None` once `is_current` turns false.
pub fn prepare_batch(
    plan: &EventPlan,
    excluded: &[Vec<u8>],
    raw_excluded: &[Vec<u8>],
    events: &[RawEvent<'_>],
    is_current: impl Fn() -> bool,
) -> Option<PreparedBatch> {
    use ffi::*;
    if !is_current() {
        return None;
    }
    let mut result = PreparedBatch { identity_invalidated: !plan.root_identity_is_valid(), ..Default::default() };
    result.full_refresh = result.identity_invalidated;
    let mut cache = EventPathCache::default();
    for (index, event) in events.iter().enumerate() {
        if index & 127 == 0 && !is_current() {
            return None;
        }
        let has = |flag: u32| event.flags & flag != 0;
        if event.id != kFSEventStreamEventIdSinceNow {
            result.latest_event_id = result.latest_event_id.max(event.id);
        }
        if has(kFSEventStreamEventFlagEventIdsWrapped) || has(kFSEventStreamEventFlagRootChanged) {
            result.full_refresh = true;
            result.identity_invalidated = true;
        }
        // Dropped events name the folder to rescan with MustScanSubDirs: that subtree (below) is
        // enough. Only without it is the extent of the loss unknown.
        if (has(kFSEventStreamEventFlagUserDropped) || has(kFSEventStreamEventFlagKernelDropped))
            && !has(kFSEventStreamEventFlagMustScanSubDirs)
        {
            result.full_refresh = true;
        }
        if has(kFSEventStreamEventFlagHistoryDone) {
            result.history_finished = true;
            continue;
        }
        // Our own index writes land in excluded folders; skip them before any filesystem work.
        if raw_path_excluded(event.path, raw_excluded) {
            continue;
        }
        let Some(path) = plan.normalized_event_path(event.path, &mut cache) else {
            if plan.event_may_affect_root(event.path) {
                result.full_refresh = true;
            }
            continue;
        };
        if excluded.iter().any(|root| contains(&path, root)) {
            continue;
        }
        let within =
            |directory: Vec<u8>| if contains(&directory, &plan.root_path) { directory } else { plan.root_path.clone() };
        if has(kFSEventStreamEventFlagMount) || has(kFSEventStreamEventFlagUnmount) {
            if plan.mount_changes_history(&path) {
                result.full_refresh = true;
                result.identity_invalidated = true;
            } else if contains(&path, &plan.root_path) {
                // Another volume appeared or vanished inside the map: the map never descends into
                // it, so only its mount point's folder changed (an unmount may uncover a folder).
                result.directories.insert(within(parent(&path).to_vec()));
            }
            continue;
        }
        if plan.untracked_mounts.iter().any(|mount| contains(&path, mount)) {
            continue;
        }
        if has(kFSEventStreamEventFlagMustScanSubDirs) {
            // Changes below `path` were coalesced: only a recursive reconciliation finds them.
            result.subtrees.insert(within(path));
            continue;
        }
        // File-level events name the changed item, so its parent's listing reconciles it: a
        // removed or replaced subfolder is pruned there, a new one is mapped in full. A folder
        // that still exists (created, permissions or attributes changed) is listed itself too.
        if has(kFSEventStreamEventFlagItemIsDir)
            && !has(kFSEventStreamEventFlagItemRemoved)
            && !has(kFSEventStreamEventFlagItemRenamed)
        {
            result.directories.insert(within(path.clone()));
        }
        result.directories.insert(within(parent(&path).to_vec()));
    }
    if !is_current() {
        return None;
    }
    if !cache.revalidate() {
        result.full_refresh = true;
    }
    if !plan.root_identity_is_valid() {
        result.full_refresh = true;
        result.identity_invalidated = true;
    }
    Some(result)
}

// --- Checkpoint identity --------------------------------------------------------------------

struct IdentityFields<'a> {
    root_path: &'a [u8],
    canonical_root: &'a [u8],
    root_file_id: u64,
    device_mode: bool,
    host_id: Option<&'a str>,
    volumes: &'a [Volume],
    boot_traversal: bool,
}

fn json_string(out: &mut String, bytes: &[u8]) {
    out.push('"');
    for ch in String::from_utf8_lossy(bytes).chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A canonical JSON fingerprint (sorted keys) of everything a saved event id depends on.
fn identity_json(fields: &IdentityFields<'_>) -> String {
    let mut out = String::from("{");
    if fields.boot_traversal {
        out.push_str("\"bootTraversal\":2,");
    }
    out.push_str("\"canonicalRoot\":");
    json_string(&mut out, fields.canonical_root);
    if let Some(host) = fields.host_id {
        out.push_str(",\"hostID\":");
        json_string(&mut out, host.as_bytes());
    }
    out.push_str(",\"mode\":");
    json_string(&mut out, if fields.device_mode { b"device" } else { b"host" });
    out.push_str(&format!(",\"rootFileID\":{}", fields.root_file_id));
    out.push_str(",\"rootPath\":");
    json_string(&mut out, fields.root_path);
    out.push_str(",\"version\":1,\"volumes\":[");
    for (index, volume) in fields.volumes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('{');
        if let Some(events) = &volume.events_uuid {
            out.push_str("\"eventsUUID\":");
            json_string(&mut out, events.as_bytes());
            out.push(',');
        }
        out.push_str("\"mountPath\":");
        json_string(&mut out, &volume.mount_path);
        out.push_str(&format!(",\"readOnly\":{}", volume.read_only));
        if let Some(uuid) = &volume.volume_uuid {
            out.push_str(",\"volumeUUID\":");
            json_string(&mut out, uuid.as_bytes());
        }
        out.push('}');
    }
    out.push_str("]}");
    out
}

fn volume(filesystem: &libc::statfs) -> Volume {
    let mount = paths::mount_on_name(filesystem);
    let device = paths::stat(&mount).map_or_else(|| paths::fsid(filesystem)[0], |info| info.st_dev);
    // SAFETY: plain FSEvents query; the returned UUID (if any) is owned and released below.
    let events_uuid = unsafe {
        let uuid = ffi::FSEventsCopyUUIDForDevice(device);
        if uuid.is_null() {
            None
        } else {
            let owned = ffi::CfOwned(uuid);
            Some(ffi::format_uuid(&ffi::CFUUIDGetUUIDBytes(owned.0).bytes))
        }
    };
    Volume {
        volume_uuid: volume_uuid(&mount),
        events_uuid,
        read_only: filesystem.f_flags & libc::MNT_RDONLY as u32 != 0,
        mount_path: mount,
    }
}

/// `ATTR_VOL_UUID` of the volume mounted at `mount`; `None` when absent or all zero.
fn volume_uuid(mount: &[u8]) -> Option<String> {
    let c = std::ffi::CString::new(mount).ok()?;
    let mut attributes = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = [0u8; 4 + 16];
    // SAFETY: valid path, attribute list and output buffer for the call.
    let status = orbit_platform::posix::retry_interrupted(|| unsafe {
        libc::getattrlist(c.as_ptr(), (&raw mut attributes).cast(), buffer.as_mut_ptr().cast(), buffer.len(), 0)
    });
    if status != 0 || u32::from_ne_bytes(buffer[..4].try_into().ok()?) < 20 {
        return None;
    }
    let uuid: [u8; 16] = buffer[4..].try_into().ok()?;
    (uuid != [0; 16]).then(|| ffi::format_uuid(&uuid))
}
