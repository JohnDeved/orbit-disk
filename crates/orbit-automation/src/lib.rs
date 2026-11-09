//! Local automation: protocol, MCP tool catalog, Unix-socket transport, Codex installer.
//!
//! - [`catalog`]: the `orbit_*` tools, their schemas and the argument validator.
//! - [`Controller`]: executes commands against an app-provided [`AutomationModel`].
//! - [`transport`]: the private Unix-domain socket between `OrbitControl` and the app.
//! - [`installer`]: Codex registration, MCP settings and the `orbitdisk` symlink.

pub mod catalog;
mod controller;
mod error;
#[cfg(unix)]
pub mod installer;
mod model;
#[cfg(unix)]
pub mod transport;

pub use controller::{Controller, MAX_PLAN_ITEMS, PAGE_LIFETIME, PLAN_LIFETIME};
pub use error::{Failure, TransportError};
pub use model::{
    AutomationModel, Clock, ItemInfo, ModelState, RemovalMode, RemovalReceipt, RemovalStart, ScanState, SessionId,
    SystemClock,
};
