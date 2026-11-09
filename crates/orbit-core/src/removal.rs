//! The app's entry point for "Move to Trash" and "Delete Permanently" (Swift
//! `DiskModel.removeCollection`): opens the cache-wide removal journal where the Swift app kept
//! it, builds the portable removal policy from the platform's protected locations, and runs
//! [`RemovalRun`] with on-demand cloud downloads disabled for the whole run.
//!
//! The returned [`RemovalOutcome`] carries the overlay views must apply until the map catches up
//! (`overlay`), the parents to refresh (`changed_parents`, or `parents_overflow` for a full
//! refresh) and the journal cursor after this run. Feed `changed_parents` to
//! [`crate::saved::SavedMaps::refresh`] (saved maps); a running [`crate::session`] replays the
//! journal itself.

use crate::ScanRemoval;
use crate::journal::{
    FailureReason, Journal, JournalOptions, NoFollowDelete, RemovalExecutor, RemovalFailure, RemovalOutcome,
    RemovalRun, TrashExecutor,
};
use crate::policy::{PlatformPolicyFs, RemovalPolicy, cloud_exclusions};
use crate::store::IndexPaths;
use orbit_platform::{Cancellation, FileIdentity, Platform};
use std::path::Path;

/// How removed items leave the disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemovalMode {
    /// [`Platform::trash`].
    Trash,
    /// Permanent, symlink-safe deletion ([`NoFollowDelete`]).
    Delete,
}

/// Opens the removal journal at `paths.journal` (creating it when missing).
pub fn open_journal<P: Platform>(platform: &P, paths: &IndexPaths) -> crate::journal::JournalResult<Journal> {
    Journal::open(
        &paths.journal,
        JournalOptions { access: crate::journal::Access::ReadWrite, aliases: platform.path_aliases() },
    )
}

/// Items collected inside one mapped root, with the identities they had when collected.
#[derive(Clone, Debug)]
pub struct RemovalRequest {
    pub root: Vec<u8>,
    pub root_identity: FileIdentity,
    pub items: Vec<ScanRemoval>,
}

/// Removes the request's items under the platform's removal policy (`home` locates the user's
/// protected folders and cloud storage). Every problem is reported as a [`RemovalFailure`].
pub fn remove<P: Platform>(
    platform: &P,
    paths: &IndexPaths,
    request: RemovalRequest,
    mode: RemovalMode,
    home: &Path,
    cancel: &Cancellation,
) -> RemovalOutcome {
    let root = request.root.clone();
    let fs = PlatformPolicyFs(platform);
    let known: Vec<Vec<u8>> = platform.cloud_roots(home).iter().map(|p| crate::discovery::path_bytes(p)).collect();
    let clouds = cloud_exclusions::<_, Vec<u8>>(&known, &[], &fs);
    let volumes: Vec<Vec<u8>> =
        platform.volumes().into_iter().filter(|v| !v.is_boot).map(|v| crate::discovery::path_bytes(&v.path)).collect();
    let home_bytes = crate::discovery::path_bytes(home);
    let protected = platform.protected_paths(home);
    let policy = |path: &[u8]| {
        // A fresh policy per item: its caches must not outlive one check (see `policy`).
        RemovalPolicy::new(&fs, &root, &home_bytes, &protected, &clouds, &volumes).rejection_reason(path)
    };
    match mode {
        RemovalMode::Trash => {
            let mut executor = TrashExecutor(|path: &Path| platform.trash(path));
            remove_with(platform, paths, request, &mut executor, policy, cancel)
        }
        RemovalMode::Delete => remove_with(platform, paths, request, &mut NoFollowDelete, policy, cancel),
    }
}

/// [`remove`] with an explicit executor and policy (tests, automation).
pub fn remove_with<P: Platform, E: RemovalExecutor>(
    platform: &P,
    paths: &IndexPaths,
    request: RemovalRequest,
    executor: &mut E,
    policy: impl FnMut(&[u8]) -> Option<String>,
    cancel: &Cancellation,
) -> RemovalOutcome {
    let failed =
        |reason| RemovalOutcome { failures: vec![RemovalFailure { path: None, reason }], ..RemovalOutcome::default() };
    let journal = match open_journal(platform, paths) {
        Ok(journal) => journal,
        Err(error) => return failed(FailureReason::NotRecorded(error)),
    };
    let run = RemovalRun::new(&journal, request.root, request.root_identity, request.items);
    match platform.without_materialization(|| run.execute(executor, policy, cancel)) {
        Ok(outcome) => outcome,
        Err(error) => failed(FailureReason::Failed(error)),
    }
}
