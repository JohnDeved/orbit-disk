//! Region notification. Each [`Regions`] bit has one [`RegionSignal`] entity. The host notifies
//! exactly the signals named by `Effect::Notify`; a panel view observes the signals of the regions
//! it draws (see [`crate::host::Host::observe`]) and re-renders only then.
//!
//! Panels are mounted as cached views (`AnyView::cached`), so a view that was not notified
//! reuses its previous frame: a progress tick re-renders the footer and nothing else.

use gpui::{App, AppContext, Entity};
use orbit_app_state::Regions;

/// Every region, in a fixed order (also the order of frame statistics).
pub const ALL: [(Regions, &str); 8] = [
    (Regions::HEADER, "header"),
    (Regions::SIDEBAR, "sidebar"),
    (Regions::MAP, "map"),
    (Regions::LIST, "list"),
    (Regions::INSPECTOR, "inspector"),
    (Regions::FOOTER, "footer"),
    (Regions::TRAY, "tray"),
    (Regions::SHEETS, "sheets"),
];

/// Position of a single region in [`ALL`].
pub fn index(region: Regions) -> usize {
    ALL.iter().position(|(r, _)| *r == region).expect("a single region")
}

pub fn name(region: Regions) -> &'static str {
    ALL[index(region)].1
}

/// The notification target of one region. Carries no state besides a counter.
#[derive(Debug)]
pub struct RegionSignal {
    pub region: Regions,
    /// How many times the host notified this region.
    pub notifications: u64,
}

/// The eight region signals. Cheap to clone (entity handles).
#[derive(Clone)]
pub struct RegionSignals {
    signals: [Entity<RegionSignal>; 8],
}

impl std::fmt::Debug for RegionSignals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegionSignals").finish_non_exhaustive()
    }
}

impl RegionSignals {
    pub fn new(cx: &mut App) -> Self {
        Self { signals: ALL.map(|(region, _)| cx.new(|_| RegionSignal { region, notifications: 0 })) }
    }

    /// The signal of a single region.
    pub fn get(&self, region: Regions) -> &Entity<RegionSignal> {
        &self.signals[index(region)]
    }

    /// Notifies the signal of every region in `regions` and no other.
    pub fn notify(&self, regions: Regions, cx: &mut App) {
        for (i, (region, _)) in ALL.iter().enumerate() {
            if regions.contains(*region) {
                self.signals[i].update(cx, |signal, cx| {
                    signal.notifications += 1;
                    cx.notify();
                });
            }
        }
    }
}
