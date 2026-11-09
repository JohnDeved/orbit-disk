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
    pub discovery_state: String,
    pub discovery_detail: &'static str,
    pub cache_fraction: Option<f64>,
    pub cache_state: &'static str,
    pub cache_detail: &'static str,
    pub is_discovering: bool,
    pub is_paused: bool,
    pub phase: Phase,
    pub action: FooterAction,
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

    /// Text of the phase at the right of the footer.
    pub fn phase_text(&self) -> &'static str {
        match self.phase {
            Phase::Finding if self.discovered_files > 0 => "Finding more files",
            Phase::Finding => "Finding files",
            Phase::Saving => "Indexing files",
            Phase::Finishing => "Finishing index",
            Phase::Saved => "Indexed",
            Phase::Updating => "Updating map",
            Phase::Paused => "Paused",
            Phase::Interrupted => "Interrupted",
        }
    }

    /// The track's help text.
    pub fn track_help(&self) -> &'static str {
        if self.saved_share().is_none() {
            "The indexed share is being updated. Stripes do not represent a percentage."
        } else {
            "Files found so far: ivory is available in memory; orange is indexed. Finding files continues until the status changes."
        }
    }
}

/// `"37% estimated"`, `"<1% estimated"`, or `"Estimating…"`.
pub fn estimated_discovery_state(fraction: f64) -> String {
    if !fraction.is_finite() {
        return "Estimating…".into();
    }
    if fraction > 0.0 && fraction < 0.01 {
        return "<1% estimated".into();
    }
    format!("{}% estimated", (fraction.clamp(0.0, 0.99) * 100.0) as u32)
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
    let (discovery_state, discovery_detail): (String, &'static str) = if checking {
        ("Checking changes".into(), "Saved map available")
    } else if complete {
        ("Complete".into(), "Discovery complete")
    } else if paused {
        ("Stopped".into(), "Progress kept")
    } else if let Some(fraction) = fraction.filter(|f| f.is_finite()) {
        let detail = match progress.map(|p| p.estimate_basis) {
            Some(EstimateBasis::PreviousMap) => "From previous map",
            Some(EstimateBasis::Revised) => "More files than expected",
            _ => "From this scan",
        };
        (estimated_discovery_state(fraction), detail)
    } else {
        ("Estimating…".into(), "Discovering files")
    };
    let progress_cache = progress.and_then(|p| p.cache_fraction);
    let (cache_state, cache_fraction): (&'static str, Option<f64>) = if checking {
        ("Updating cache", None)
    } else if input.finished {
        ("Saved", Some(1.0))
    } else if input.failed {
        ("Interrupted", progress_cache)
    } else if paused {
        ("Stopped", progress_cache)
    } else if cache_finishing {
        ("Finishing cache", progress_cache)
    } else if input.cached_files.is_none() {
        ("Reading cache", progress_cache)
    } else {
        ("Saving changes", progress_cache)
    };
    let cache_detail = if input.failed {
        "Retry mapping"
    } else if !input.finished && !paused && cache_fraction.is_some() {
        "Known changes"
    } else {
        ""
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
        discovery_state,
        discovery_detail,
        cache_fraction,
        cache_state,
        cache_detail,
        is_discovering,
        is_paused: paused,
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
        assert_eq!(caught_up.phase_text(), "Finding more files");
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
        assert_eq!((backlog.phase, backlog.discovery_state.as_str()), (Phase::Saving, "Complete"));
        assert_eq!((backlog.phase_text(), backlog.saved_share()), ("Indexing files", Some(0.4)));
        assert_eq!(estimated_discovery_state(0.004), "<1% estimated");
        assert_eq!(estimated_discovery_state(1.0), "99% estimated");
        assert_eq!(estimated_discovery_state(f64::NAN), "Estimating…");
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
        assert_eq!((checking.discovery_state.as_str(), checking.cache_state), ("Checking changes", "Updating cache"));
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
            (stopped.phase, stopped.action, stopped.discovery_state.as_str()),
            (Phase::Paused, FooterAction::Resume, "Stopped")
        );
        let failed = footer(FooterInputs { failed: true, ..FooterInputs::default() });
        assert_eq!((failed.phase, failed.cache_detail), (Phase::Interrupted, "Retry mapping"));
        let saved = footer(FooterInputs { finished: true, cached_files: Some(0), ..FooterInputs::default() });
        assert_eq!((saved.phase, saved.saved_share()), (Phase::Saved, Some(1.0)));
    }
}
