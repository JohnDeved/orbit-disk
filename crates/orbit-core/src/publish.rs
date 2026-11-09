//! Publishes a finished stage as the saved map. Ported from `WALPublisher.swift`.
//!
//! Under the root mutation lease: check the invalidation generation and the saved-map
//! expectation, seal the stage, checkpoint (`TRUNCATE`) and switch it to `journal_mode=DELETE`,
//! clone it with [`Platform::clone_file`] to a hidden temporary in the cache, verify the clone is a
//! private regular file (`0600`, owned by us, `nlink == 1`), re-validate root, lease, expectation
//! and generation, then rename it over the saved map inside the caller's fence (the removal
//! journal's). Any failure before the rename removes our temporary (only if it is still the file
//! we created) and returns the stage to WAL. A crash leaves at most a hidden temporary clone,
//! which is never cleaned automatically: another process may own it.

use crate::lease::{RootMutationLease, SnapshotExpectation};
use crate::schema::SnapshotInfo;
use crate::stage::Stage;
use crate::{CoreError, Result};
use orbit_platform::Platform;
use std::time::Duration;

/// What a publication boundary hook asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryAction {
    Continue,
    /// Stop right here as if the process died: no cleanup, no WAL restore (crash tests).
    Crash,
}

/// Runs the rename inside an external fence; must call the closure at most once.
pub type Fence<'a> = &'a mut dyn FnMut(&mut dyn FnMut() -> Result<()>) -> Result<()>;
/// Called with `"cloned"` and `"renamed"`.
pub type Boundary<'a> = &'a mut dyn FnMut(&str) -> BoundaryAction;

pub struct PublishOptions<'a> {
    pub fence: Option<Fence<'a>>,
    pub boundary: Option<Boundary<'a>>,
    pub checkpoint_timeout: Duration,
}

impl Default for PublishOptions<'_> {
    fn default() -> Self {
        Self { fence: None, boundary: None, checkpoint_timeout: Duration::from_secs(5) }
    }
}

impl std::fmt::Debug for PublishOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishOptions").field("checkpoint_timeout", &self.checkpoint_timeout).finish_non_exhaustive()
    }
}

/// Internal marker for a simulated crash.
struct Crashed;

impl<P: Platform> Stage<P> {
    /// Seals and publishes the finished stage. `elapsed` is the mapping duration in seconds.
    pub fn publish(&mut self, elapsed: f64, mut options: PublishOptions<'_>) -> Result<SnapshotInfo> {
        let cancel = self.cancellation().clone();
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        self.validate_root()?;
        let paths = self.paths().clone();
        let root = self.root().to_vec();
        crate::lease::create_private_dir(&paths.locks)?;
        let lease = RootMutationLease::acquire(&paths.locks, &root, &cancel)?;
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let generation = self.writer_ref().scan_generation()?;
        if generation.as_deref() != Some(lease.invalidation_generation()?.as_str()) {
            return Err(CoreError::SnapshotChanged);
        }
        let expected = self.expectation().clone();
        let current = SnapshotExpectation::capture(&paths.cache, &root)?;
        let effective = if current == SnapshotExpectation::Absent && expected != SnapshotExpectation::Absent {
            self.writer().rebase_expectation(&expected, &SnapshotExpectation::Absent)?;
            SnapshotExpectation::Absent
        } else if current != expected {
            return Err(CoreError::SnapshotChanged);
        } else {
            expected
        };
        let checkpoint = self.checkpoint().clone();
        let prepared = self.writer().seal(&checkpoint.identity, checkpoint.event_id, elapsed)?;
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        self.writer().checkpoint_for_clone(options.checkpoint_timeout, &cancel)?;
        let source = self.lease().database_path().to_path_buf();
        let destination = crate::store::saved_map_path(&paths.cache, &root);
        let temporary = paths.cache.join(format!(".orbit-stage-{}.sqlite", uuid::Uuid::new_v4().hyphenated()));
        let mut created: Option<(u64, u64)> = None;
        let outcome = (|| -> std::result::Result<SnapshotInfo, std::result::Result<CoreError, Crashed>> {
            let fail = |e: CoreError| Ok::<CoreError, Crashed>(e);
            if cancel.is_cancelled() {
                return Err(fail(CoreError::Cancelled));
            }
            crate::lease::create_private_dir(&paths.cache).map_err(fail)?;
            self.platform().clone_file(&source, &temporary).map_err(|e| fail(e.into()))?;
            let copied = crate::lease::sys::lstat(&temporary).map_err(fail)?;
            let euid = crate::lease::sys::euid();
            if let Some(info) = copied
                && info.is_regular()
                && info.uid == euid
            {
                created = Some((info.dev, info.ino));
            }
            if !copied.is_some_and(|i| i.is_regular() && i.uid == euid && i.nlink == 1 && i.permissions() == 0o600) {
                return Err(fail(CoreError::Invalid("unsafe stage copy".into())));
            }
            if let Some(boundary) = options.boundary.as_mut()
                && boundary("cloned") == BoundaryAction::Crash
            {
                return Err(Err(Crashed));
            }
            if cancel.is_cancelled() {
                return Err(fail(CoreError::Cancelled));
            }
            self.validate_root().map_err(fail)?;
            lease.validate().map_err(fail)?;
            effective.verify(&paths.cache, &root).map_err(fail)?;
            if generation.as_deref() != Some(lease.invalidation_generation().map_err(fail)?.as_str()) {
                return Err(fail(CoreError::SnapshotChanged));
            }
            let mut rename = || -> Result<()> {
                if cancel.is_cancelled() {
                    return Err(CoreError::Cancelled);
                }
                std::fs::rename(&temporary, &destination)?;
                Ok(())
            };
            match options.fence.as_mut() {
                Some(fence) => fence(&mut rename).map_err(fail)?,
                None => rename().map_err(fail)?,
            }
            created = None;
            if let Some(boundary) = options.boundary.as_mut()
                && boundary("renamed") == BoundaryAction::Crash
            {
                return Err(Err(Crashed));
            }
            // The saved map is already committed; a failed marker only postpones stage cleanup.
            let _ = self.writer().mark_published(&prepared.id);
            Ok(SnapshotInfo { database: destination.clone(), ..prepared.clone() })
        })();
        match outcome {
            Ok(snapshot) => Ok(snapshot),
            Err(Err(Crashed)) => Err(CoreError::Cancelled),
            Err(Ok(error)) => {
                if let Some((dev, ino)) = created
                    && let Ok(Some(info)) = crate::lease::sys::lstat(&temporary)
                    && info.dev == dev
                    && info.ino == ino
                {
                    let _ = std::fs::remove_file(&temporary);
                }
                if let Err(resume) = self.writer().resume_wal() {
                    return Err(CoreError::Invalid(format!(
                        "publication failed ({error}) and the stage could not resume ({resume})"
                    )));
                }
                Err(error)
            }
        }
    }
}
