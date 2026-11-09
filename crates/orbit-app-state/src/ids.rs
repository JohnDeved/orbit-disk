//! Tokens and identifiers. Every asynchronous request is keyed by a [`RequestId`]; the state keeps
//! the generation/browse/session/epoch/snapshot tokens it captured for that request and drops the
//! reply when any of them changed.

pub use orbit_automation::SessionId;

macro_rules! token {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u64);
    };
}

token!(
    /// Identifies one backend request or compute job. Replies carry it back.
    RequestId
);
token!(
    /// Identifies one in-app collection drag. Single use: a drop consumes it, a new drag or a
    /// root change invalidates it.
    DragId
);
token!(
    /// Identifies one pointer gesture (press .. release) on the map or a row.
    GestureId
);
token!(
    /// Identifies one removal run (Trash or permanent delete).
    RemovalId
);
token!(
    /// A confirmation lease held while the permanent-delete sheet is open.
    LeaseId
);

/// Deterministic id source. The host seeds it randomly; tests use a fixed seed.
#[derive(Clone, Debug)]
pub(crate) struct IdGen {
    seed: u64,
    next: u64,
}

impl IdGen {
    pub fn new(seed: u64) -> Self {
        Self { seed, next: 1 }
    }

    pub fn next(&mut self) -> u64 {
        let value = self.next;
        self.next += 1;
        value
    }

    pub fn session(&mut self) -> SessionId {
        let n = self.next();
        uuid::Uuid::from_u64_pair(self.seed, n)
    }
}
