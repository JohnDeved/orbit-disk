//! UI-agnostic application state machine for Orbit Disk: sessions, folders, hover relations,
//! selection, search, sorting, collection, removal, mapping footer, places and volumes.
//!
//! [`AppState::handle`] is deterministic: no threads, timers or I/O. It returns [`Effect`]s the
//! host performs ([`backend::perform`] on a background executor, timers, platform actions,
//! persistence) and a final [`Effect::Notify`] naming the UI regions that changed. Results come
//! back as [`Event`]s carrying the [`ids::RequestId`] they answer; stale results are dropped.

pub mod automation;
pub mod backend;
pub mod collection;
pub mod coverage;
pub mod event;
pub mod folder;
pub mod footer;
pub mod gesture;
mod hash;
pub mod ids;
pub mod model;
pub mod paths;
pub mod places;
pub mod review;
mod session;
mod state;
pub mod testing;

pub use backend::{Backend, Notifier, Reply, Request, perform};
pub use event::{DragSource, Effect, Event, MappingEvent, Regions, Timer, TimerKind};
pub use folder::{PreparedFolder, prepare_folder};
pub use footer::{FooterAction, FooterState, MapProgress, Phase};
pub use ids::{DragId, GestureId, LeaseId, RemovalId, RequestId, SessionId};
pub use model::{Item, SnapshotInfo, SortOrder, Volume};
pub use review::ReviewToken;
pub use state::{AppState, BACKGROUND_EDIT_ITEMS, Config, HoverPresentation, Receipt, RowView};

#[cfg(test)]
mod tests;
