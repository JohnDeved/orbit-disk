//! Collection review tokens (Swift `CollectionReviewToken`).

use std::sync::Arc;

use orbit_platform::FileIdentity;

use crate::ids::SessionId;

/// Issued when the review sheet opens. Actions from the sheet carry it:
/// - *owner* (session, root, identity, review instance) must match for any action;
/// - the *collection revision* must also match for actions on the reviewed contents
///   (removal, clear, row removal), so a changed collection makes a stale sheet a no-op;
/// - the *error revision* decides whether closing the sheet may clear the error it showed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReviewToken {
    pub(crate) session: SessionId,
    pub(crate) root: Arc<[u8]>,
    pub(crate) root_identity: FileIdentity,
    pub(crate) review: u64,
    pub(crate) collection_revision: u64,
    pub(crate) error_revision: u64,
}

impl ReviewToken {
    pub fn session(&self) -> SessionId {
        self.session
    }
}
