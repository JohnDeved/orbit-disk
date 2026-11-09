//! Orbit Disk scan engine.
//!
//! Data flow: `discovery` workers list directories through the platform layer and publish
//! [`records::RecordBatch`]es into the live inventory immediately and into the [`mailbox`]
//! for durability. The single `stage` writer drains the mailbox into SQLite, `publish` turns
//! a finished stage into a saved map, and `store` serves saved maps. `search`, `projection`,
//! `journal` and `recovery` sit on top.
//!
//! Module ownership during the port (one owner per module; shared seams are integrator-owned):
//! - integrator (fixed seams): `types`, `records`, `mailbox`, `view`, `fold`, `paths`
//! - discovery: `discovery`, `live`, `scope`, `hardlinks`
//! - durable index: `schema`, `stage`, `pending`, `publish`, `lease`, `store`
//! - algorithms: `search`, `collate`, `projection`, `progress`, `refresh`, `policy`
//! - removal: `journal`, `recovery`

pub mod fold;
pub mod mailbox;
pub mod paths;
pub mod records;
pub mod types;
pub mod view;

// Discovery & live inventory.
pub mod discovery;
pub mod hardlinks;
pub mod live;
pub mod scope;

// Durable index.
pub mod lease;
pub mod pending;
pub mod publish;
pub mod schema;
pub mod stage;
pub mod store;

// Algorithms.
pub mod collate;
pub mod policy;
pub mod progress;
pub mod projection;
pub mod refresh;
pub mod search;

// Removal.
pub mod journal;
pub mod recovery;

// Integration: what the app drives.
pub mod query;
pub mod removal;
pub mod reuse;
pub mod saved;
pub mod session;

pub use types::{CoreError, Result, ScanRemoval};
