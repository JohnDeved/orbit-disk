//! Mapping-footer progress: what fraction of discovery is done and on what basis.
//!
//! Ported from `MappingProgress.swift`. Discovery counters come from
//! [`crate::mailbox::DiscoveryProgress`]; time is passed in as seconds on any monotonic clock so the
//! estimator has no clock of its own.
//!
//! The estimator never claims completion it cannot confirm: cancelled or uncertain work (failures,
//! deferred retries) has no fraction, a growing frontier has no stable total, and only a confirmed
//! finish reports `1.0`. A previous complete map of the same root gives a baseline until the scan
//! outgrows it. When new work moves the estimate backwards, the basis reads
//! [`EstimateBasis::Revised`] for two seconds.

use crate::mailbox::DiscoveryProgress;

/// Estimates never exceed this before discovery is confirmed complete.
const MAX_ESTIMATE: f64 = 0.98;
/// Directories that must complete before the frontier extrapolation is trusted.
const MIN_COMPLETED_DIRECTORIES: u64 = 8;
/// Child directories per completed directory at or above which the frontier is still growing.
const GROWING_FRONTIER: f64 = 0.9;
/// A drop larger than this marks the estimate as revised.
const REVISION_TOLERANCE: f64 = 0.025;
/// Seconds the "revised" basis stays visible.
const REVISED_SECONDS: f64 = 2.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum EstimateBasis {
    /// No trustworthy estimate yet.
    #[default]
    Estimating,
    /// Extrapolated from this scan's completed directories (or confirmed complete).
    CurrentScan,
    /// Relative to the file count of the previous complete map.
    PreviousMap,
    /// Recently moved backwards because more work was found.
    Revised,
}

/// What the mapping footer shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MappingProgress {
    /// Changes whenever anything shown changes.
    pub revision: u64,
    /// Files already saved to the durable index, when known exactly.
    pub cached_file_count: Option<u64>,
    pub discovery_fraction: Option<f64>,
    pub discovery_complete: bool,
    pub estimate_basis: EstimateBasis,
    /// Share of discovered work already durable ([`DiscoveryProgress::cache_fraction`]).
    pub cache_fraction: Option<f64>,
    /// Discovery is done and only saving remains.
    pub cache_finishing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    pub fraction: Option<f64>,
    pub basis: EstimateBasis,
}

#[derive(Clone, Debug, Default)]
pub struct DiscoveryEstimator {
    previous_file_count: Option<u64>,
    last_fraction: Option<f64>,
    revised_until: f64,
}

impl DiscoveryEstimator {
    /// `previous_file_count`: files in the last complete map of this root (zero means none).
    pub fn new(previous_file_count: Option<u64>) -> Self {
        Self { previous_file_count: previous_file_count.filter(|n| *n > 0), ..Self::default() }
    }

    pub fn previous_file_count(&self) -> Option<u64> {
        self.previous_file_count
    }

    /// `complete`: discovery is confirmed finished (all retries reconciled). `now`: seconds on a
    /// monotonic clock.
    pub fn estimate(&mut self, work: &DiscoveryProgress, complete: bool, now: f64) -> Estimate {
        if work.cancelled {
            return Estimate { fraction: None, basis: EstimateBasis::Estimating };
        }
        if complete {
            self.last_fraction = Some(1.0);
            return Estimate { fraction: Some(1.0), basis: EstimateBasis::CurrentScan };
        }
        if work.uncertain {
            return Estimate { fraction: None, basis: EstimateBasis::Estimating };
        }
        let mut fraction = None;
        let mut basis = EstimateBasis::Estimating;
        match self.previous_file_count {
            Some(previous) if work.files < previous => {
                fraction = Some(MAX_ESTIMATE.min(work.files as f64 / previous as f64));
                basis = EstimateBasis::PreviousMap;
            }
            _ => {
                let remaining = work.active_directories + work.queued_directories;
                // The recent branching rate can forecast unseen descendants, but a growing frontier
                // has no stable total.
                if work.completed_directories >= MIN_COMPLETED_DIRECTORIES
                    && remaining > 0
                    && work.recent_children < GROWING_FRONTIER
                {
                    let remaining = remaining as f64 / (1.0 - work.recent_children).max(0.1);
                    let completed = work.completed_directories as f64;
                    fraction = Some(MAX_ESTIMATE.min(completed / (completed + remaining)));
                    basis = EstimateBasis::CurrentScan;
                }
                if self.previous_file_count.is_some() {
                    // The previous map was outgrown: say so instead of plateauing near 100%.
                    self.revised_until = now + REVISED_SECONDS;
                }
            }
        }
        if let (Some(f), Some(previous)) = (fraction, self.last_fraction)
            && f < previous - REVISION_TOLERANCE
        {
            self.revised_until = now + REVISED_SECONDS;
        }
        if fraction.is_some() {
            self.last_fraction = fraction;
        }
        if now < self.revised_until {
            basis = EstimateBasis::Revised;
        }
        Estimate { fraction, basis }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_revise_and_never_invent_totals() {
        let mut cold = DiscoveryEstimator::new(None);
        let mut work = DiscoveryProgress::default();
        assert_eq!(cold.estimate(&work, false, 0.0).fraction, None, "empty discovery invented a total");
        work.files = 100_000;
        work.active_directories = 1;
        assert_eq!(cold.estimate(&work, false, 0.0).fraction, None, "a single flat listing invented a total");

        work.files = 128;
        work.completed_directories = 8;
        work.queued_directories = 4;
        work.recent_children = 0.2;
        let first = cold.estimate(&work, false, 1.0);
        let f = first.fraction.unwrap();
        assert!(f > 0.0 && f < 1.0 && first.basis == EstimateBasis::CurrentScan);
        for files in [0, 1, 63] {
            let sampled = DiscoveryEstimator::new(None).estimate(&DiscoveryProgress { files, ..work }, false, 1.0);
            assert_eq!(sampled, first, "directory work stayed indeterminate because it held few files");
        }
        work.queued_directories = 200;
        let revised = cold.estimate(&work, false, 2.0);
        assert!(revised.fraction.unwrap() < f && revised.basis == EstimateBasis::Revised);
        assert_eq!(cold.estimate(&work, false, 4.5).basis, EstimateBasis::CurrentScan, "revision did not expire");
        work.recent_children = 1.2;
        assert_eq!(cold.estimate(&work, false, 5.0).fraction, None, "a growing frontier claimed a stable total");

        let mut warm = DiscoveryEstimator::new(Some(1_000));
        work.files = 500;
        assert_eq!(
            warm.estimate(&work, false, 0.0),
            Estimate { fraction: Some(0.5), basis: EstimateBasis::PreviousMap }
        );
        work.files = 1_001;
        assert_eq!(warm.estimate(&work, false, 1.0), Estimate { fraction: None, basis: EstimateBasis::Revised });
        work.finished = true;
        work.uncertain = true;
        assert_eq!(warm.estimate(&work, false, 2.0).fraction, None, "completion hid pending retries");
        assert_eq!(warm.estimate(&work, true, 3.0).fraction, Some(1.0));
        work.cancelled = true;
        assert_eq!(warm.estimate(&work, true, 4.0).fraction, None, "cancellation shown as completion");
        assert_eq!(DiscoveryEstimator::new(Some(0)).estimate(&DiscoveryProgress::default(), false, 0.0).fraction, None);

        // Cache-import uncertainty hides the cache denominator but not memory discovery progress.
        let imported = DiscoveryProgress { files: 9_000, cache_uncertain: true, ..DiscoveryProgress::default() };
        assert_eq!(DiscoveryEstimator::new(Some(10_000)).estimate(&imported, false, 0.0).fraction, Some(0.9));
    }
}
