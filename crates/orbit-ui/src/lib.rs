//! GPUI views for Orbit Disk.
//!
//! - [`host`]: [`host::AppHost`] owns the `AppState` and performs its effects; panels hold a
//!   [`host::Host`] handle.
//! - [`regions`]: one signal entity per UI region; `Effect::Notify` notifies only those.
//! - [`main_window`]: window options, layout skeleton and window actions; [`panels`] and
//!   [`sheets`]: one view per region.
//! - [`theme`], [`icons`], [`components`]: design tokens, embedded icons, shared widgets;
//!   [`format`]: byte, percent and count formatting (Foundation-compatible).
//! - [`actions`]: menu bar and key bindings; [`lifecycle`]: quit/close guards;
//!   [`settings`]/[`settings_window`]: persisted settings and the Settings window.
//! - [`frame_stats`] and [`harness`]: frame instrumentation and the synthetic-input harness.
//! - [`fixture`]: a generated in-memory backend for `--fixture` runs; [`marketing`]: drives it
//!   into the README screenshot states (`--marketing-state`).

pub mod actions;
pub mod components;
pub mod fixture;
pub mod format;
pub mod frame_stats;
pub mod harness;
pub mod host;
pub mod icons;
pub mod lifecycle;
pub mod main_window;
pub mod marketing;
pub mod native_window;
pub mod panels;
pub mod regions;
pub mod settings;
pub mod settings_window;
pub mod sheets;
pub mod text_edit;
pub mod theme;

pub use host::{AppHost, Host, HostEvent, HostOptions};
pub use icons::Assets;

#[cfg(test)]
mod tests;
