//! Mapping footer values (Swift `refreshMappingProgress` + `MappingProgressView`). Kept apart from
//! the folder state: progress ticks change only this and notify only the footer.

use crate::model::{EstimateBasis, ProgressSnapshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Finding,
    Saving,
    Finishing,
    Saved,
    Updating,
    Paused,
    Interrupted,
}

/// The footer button at the right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FooterAction {
    /// "Stop": stop mapping this location.
    Stop,
    /// "Resume": continue mapping this location.
    Resume,
    /// "Refresh": refresh this map and restart updates.
    Refresh,
    /// "Pause": pause the current refresh.
    Pause,
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FooterState {
    pub root_name: String,
    pub discovered_files: u64,
    /// `None` while the indexed count is not comparable (updating, mismatched snapshot).
    pub cached_files: Option<u64>,
    pub discovery_fraction: Option<f64>,
    pub discovery_detail: &'static str,
    pub is_paused: bool,
    /// Every file has been found (the found count is final).
    pub discovery_complete: bool,
    /// A location with a saved map is mapped again while that map stays available.
    pub rescan: bool,
    pub phase: Phase,
    pub action: FooterAction,
}

/// Progress towards a finished map, both shares measured against the files expected in total, so
/// a full track means the map is complete. `indexed <= found <= 1`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapProgress {
    /// Files found over the expected total: an estimate (below 1) until discovery completes.
    pub found: f64,
    /// Files indexed over the expected total; `None` while the indexed count is withheld.
    pub indexed: Option<f64>,
}

impl MapProgress {
    /// The share that counts as done: indexed when known, otherwise found.
    pub fn done(&self) -> f64 {
        self.indexed.unwrap_or(self.found)
    }
}

impl FooterState {
    /// Coral share of the ivory track: indexed files over files found so far. `None` (striped)
    /// when the counts are not comparable. Never exceeds the found count.
    pub fn saved_share(&self) -> Option<f64> {
        let cached = self.cached_files?;
        if cached > self.discovered_files {
            return None;
        }
        if self.discovered_files == 0 {
            return (self.phase == Phase::Saved).then_some(1.0);
        }
        Some((cached as f64 / self.discovered_files as f64).clamp(0.0, 1.0))
    }

    /// Progress towards the finished map, `None` without an estimate of the total (a first map of
    /// a location before discovery has an estimate) or once there is nothing left to do.
    pub fn map_progress(&self) -> Option<MapProgress> {
        // Only mapping work has a total: an update, a saved map or paused updates have none.
        let mapping = match self.phase {
            Phase::Finding | Phase::Saving | Phase::Finishing | Phase::Interrupted => true,
            Phase::Paused => self.is_paused,
            Phase::Saved | Phase::Updating => false,
        };
        if !mapping {
            return None;
        }
        let found = if self.discovery_complete {
            1.0
        } else {
            // An estimate never claims discovery finished.
            self.discovery_fraction.filter(|f| f.is_finite() && *f > 0.0)?.clamp(0.0, 0.99)
        };
        let indexed = self.saved_share().map(|share| (share * found).clamp(0.0, found));
        Some(MapProgress { found, indexed })
    }

    /// The expected total number of files, from the discovery estimate (rounded; never below the
    /// files already found). `None` without an estimate.
    pub fn expected_files(&self) -> Option<u64> {
        if self.discovery_complete {
            return Some(self.discovered_files);
        }
        let fraction = self.discovery_fraction.filter(|f| f.is_finite() && *f > 0.0)?;
        Some(((self.discovered_files as f64 / fraction.min(1.0)).round() as u64).max(self.discovered_files))
    }
}

/// Everything the footer depends on, gathered by the state machine.
#[derive(Clone, Debug, Default)]
pub(crate) struct FooterInputs<'a> {
    pub root_name: String,
    pub progress: Option<&'a ProgressSnapshot>,
    pub discovered_files: u64,
    pub cached_files: Option<u64>,
    pub finished: bool,
    pub failed: bool,
    pub stopped: bool,
    pub refreshing: bool,
    pub scanning: bool,
    pub partial: bool,
    pub updates_paused: bool,
    pub pending_removal: bool,
    pub pending_refresh: bool,
    pub pending_presentation: bool,
}

pub(crate) fn compute(input: &FooterInputs<'_>) -> FooterState {
    let progress = input.progress;
    let checking = input.finished && input.refreshing;
    let updating = input.pending_removal
        || (input.finished && (checking || input.pending_refresh))
        || (input.finished && input.pending_presentation);
    let paused = !input.finished && (input.stopped || input.failed);
    let complete = input.finished || progress.is_some_and(|p| p.discovery_complete);
    let cache_finishing = progress.is_some_and(|p| p.cache_finishing);
    let phase = if input.failed && !input.finished {
        Phase::Interrupted
    } else if paused || (input.updates_paused && updating) {
        Phase::Paused
    } else if !complete {
        Phase::Finding
    } else if updating {
        Phase::Updating
    } else if input.finished {
        Phase::Saved
    } else if cache_finishing {
        Phase::Finishing
    } else {
        Phase::Saving
    };
    let cached_files = if updating || input.cached_files.is_some_and(|c| c > input.discovered_files) {
        None
    } else {
        input.cached_files
    };
    let fraction = if complete { Some(1.0) } else { progress.and_then(|p| p.discovery_fraction) };
    let discovery_detail: &'static str = if checking {
        "Saved map available"
    } else if complete {
        "Discovery complete"
    } else if paused {
        "Progress kept"
    } else if fraction.is_some_and(f64::is_finite) {
        match progress.map(|p| p.estimate_basis) {
            Some(EstimateBasis::PreviousMap) => "From previous map",
            Some(EstimateBasis::Revised) => "More files than expected",
            _ => "From this scan",
        }
    } else {
        "Discovering files"
    };
    let is_discovering = !complete && !paused;
    let action = if is_discovering {
        FooterAction::Stop
    } else if paused {
        FooterAction::Resume
    } else if phase == Phase::Paused {
        FooterAction::Refresh
    } else if input.refreshing && !input.scanning && !input.partial {
        FooterAction::Pause
    } else {
        FooterAction::None
    };

    FooterState {
        root_name: input.root_name.clone(),
        discovered_files: input.discovered_files,
        cached_files,
        discovery_fraction: fraction,
        discovery_detail,
        is_paused: paused,
        discovery_complete: complete,
        rescan: input.refreshing && !input.finished && !input.scanning && !input.partial,
        phase,
        action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn footer(input: FooterInputs<'_>) -> FooterState {
        compute(&input)
    }

    #[test]
    fn indexed_share_never_implies_discovery_finished_or_exceeds_found() {
        let progress = ProgressSnapshot { discovery_fraction: Some(0.4), ..ProgressSnapshot::default() };
        // A full coral bar while finding files means the cache caught up, not that discovery ended.
        let caught_up = footer(FooterInputs {
            progress: Some(&progress),
            discovered_files: 100,
            cached_files: Some(100),
            ..FooterInputs::default()
        });
        assert_eq!(caught_up.phase, Phase::Finding);
        assert_eq!(caught_up.saved_share(), Some(1.0));
        assert_eq!(caught_up.action, FooterAction::Stop);
        // Incompatible counts (more indexed than found) are withheld instead of showing 100%.
        let mismatched = footer(FooterInputs {
            progress: Some(&progress),
            discovered_files: 10,
            cached_files: Some(50),
            ..FooterInputs::default()
        });
        assert_eq!(mismatched.cached_files, None);
        assert_eq!(mismatched.saved_share(), None);
        // Discovery finished while the cache writer is behind: complete, still indexing.
        let complete = ProgressSnapshot { discovery_complete: true, ..ProgressSnapshot::default() };
        let backlog = footer(FooterInputs {
            progress: Some(&complete),
            discovered_files: 100,
            cached_files: Some(40),
            ..FooterInputs::default()
        });
        assert_eq!((backlog.phase, backlog.discovery_detail), (Phase::Saving, "Discovery complete"));
        assert_eq!(backlog.saved_share(), Some(0.4));
    }

    #[test]
    fn pending_updates_withhold_counts_and_paused_updates_offer_refresh() {
        let updating = footer(FooterInputs {
            discovered_files: 10,
            cached_files: Some(10),
            finished: true,
            pending_removal: true,
            ..FooterInputs::default()
        });
        assert_eq!(updating.phase, Phase::Updating);
        assert_eq!(updating.cached_files, None);
        let checking = footer(FooterInputs { finished: true, refreshing: true, ..FooterInputs::default() });
        assert_eq!((checking.phase, checking.discovery_detail), (Phase::Updating, "Saved map available"));
        assert_eq!(checking.action, FooterAction::Pause);
        let paused = footer(FooterInputs {
            finished: true,
            pending_refresh: true,
            updates_paused: true,
            ..FooterInputs::default()
        });
        assert_eq!((paused.phase, paused.action), (Phase::Paused, FooterAction::Refresh));
        let stopped = footer(FooterInputs { stopped: true, discovered_files: 5, ..FooterInputs::default() });
        assert_eq!(
            (stopped.phase, stopped.action, stopped.discovery_detail),
            (Phase::Paused, FooterAction::Resume, "Progress kept")
        );
        let failed = footer(FooterInputs { failed: true, ..FooterInputs::default() });
        assert_eq!((failed.phase, failed.action), (Phase::Interrupted, FooterAction::Resume));
        let saved = footer(FooterInputs { finished: true, cached_files: Some(0), ..FooterInputs::default() });
        assert_eq!((saved.phase, saved.saved_share()), (Phase::Saved, Some(1.0)));
    }

    /// The track measures against the expected total: full means finished, an estimate never
    /// reaches it, and the indexed share never passes the found share.
    #[test]
    fn map_progress_is_measured_against_the_expected_total() {
        let estimate = ProgressSnapshot { discovery_fraction: Some(0.5), ..ProgressSnapshot::default() };
        let finding = footer(FooterInputs {
            progress: Some(&estimate),
            discovered_files: 100,
            cached_files: Some(50),
            ..FooterInputs::default()
        });
        assert_eq!(finding.map_progress(), Some(MapProgress { found: 0.5, indexed: Some(0.25) }));
        assert_eq!(finding.expected_files(), Some(200));
        // More files than the estimate expected: still below full while discovery runs.
        let over = ProgressSnapshot { discovery_fraction: Some(1.3), ..ProgressSnapshot::default() };
        let more = footer(FooterInputs {
            progress: Some(&over),
            discovered_files: 130,
            cached_files: Some(130),
            ..FooterInputs::default()
        });
        assert_eq!(more.map_progress().map(|p| p.found), Some(0.99));
        assert_eq!(more.expected_files(), Some(130), "never below the files found");
        // No estimate yet: no percentage is invented.
        let unknown = footer(FooterInputs { discovered_files: 10, ..FooterInputs::default() });
        assert_eq!(unknown.map_progress(), None);
        // Discovery complete, indexing behind: found is final, indexed catches up.
        let complete = ProgressSnapshot { discovery_complete: true, ..ProgressSnapshot::default() };
        let indexing = footer(FooterInputs {
            progress: Some(&complete),
            discovered_files: 100,
            cached_files: Some(40),
            ..FooterInputs::default()
        });
        assert_eq!(indexing.map_progress(), Some(MapProgress { found: 1.0, indexed: Some(0.4) }));
        assert_eq!(indexing.map_progress().map(|p| p.done()), Some(0.4));
        // Nothing to show once saved or while an update withholds the counts.
        let saved = footer(FooterInputs {
            finished: true,
            cached_files: Some(5),
            discovered_files: 5,
            ..FooterInputs::default()
        });
        assert_eq!(saved.map_progress(), None);
        let updates_paused = footer(FooterInputs {
            finished: true,
            pending_refresh: true,
            updates_paused: true,
            discovered_files: 5,
            ..FooterInputs::default()
        });
        assert_eq!((updates_paused.phase, updates_paused.map_progress()), (Phase::Paused, None));
        // A refresh that keeps the saved map is a rescan; a first map is not.
        let rescan = footer(FooterInputs { refreshing: true, discovered_files: 3, ..FooterInputs::default() });
        assert!(rescan.rescan && !finding.rescan);
    }
}
