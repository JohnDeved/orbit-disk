//! Which directories a mapping may descend into (Swift `VolumeTraversalScope`).
//!
//! Traversal stays on the mapped root's filesystem and never crosses mount points, with one
//! exception: on the macOS boot volume the writable Data volume is reachable twice, through the
//! logical firmlinks (`/Users`, …) and through its physical mount (`/System/Volumes/Data`). The
//! scope admits the Data volume at both places but excludes the physical paths that duplicate a
//! logical alias, so every entry is counted once. The OS-specific facts arrive as a
//! [`VolumeLayout`] from [`orbit_platform::Platform::volume_layout`]; this module is portable.

use crate::paths::{SEP, is_within};
use orbit_platform::{BootLayout, FileIdentity, VolumeLayout};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraversalScope {
    root_path: Box<[u8]>,
    root_identity: FileIdentity,
    root_mount: Option<String>,
    boot: Option<BootLayout>,
}

impl TraversalScope {
    pub fn new(root_path: &[u8], root_identity: FileIdentity, layout: VolumeLayout) -> Self {
        Self { root_path: root_path.into(), root_identity, root_mount: layout.root_mount, boot: layout.boot }
    }

    /// A scope without volume aliases: same device as the root, no mount points.
    pub fn single_volume(root_path: &[u8], root_identity: FileIdentity) -> Self {
        Self::new(root_path, root_identity, VolumeLayout::default())
    }

    pub fn root_path(&self) -> &[u8] {
        &self.root_path
    }

    pub fn root_identity(&self) -> FileIdentity {
        self.root_identity
    }

    pub fn boot(&self) -> Option<&BootLayout> {
        self.boot.as_ref()
    }

    /// Decisions depend on the path (not just device and mount flag). When false, callers may
    /// skip building child paths for scope checks: every path below the root behaves alike.
    #[inline]
    pub fn is_path_sensitive(&self) -> bool {
        self.boot.is_some()
    }

    /// May traversal descend into the directory at `path` (a directory record with `dev` and
    /// mount flag as listed in its parent)?
    pub fn allows(&self, path: &[u8], dev: u64, is_mount_point: bool) -> bool {
        if !is_within(path, &self.root_path) || self.excludes(path) {
            return false;
        }
        if is_mount_point {
            return self.boot.as_ref().is_some_and(|boot| path == &boot.data_path[..] && boot.data_identity.dev == dev);
        }
        dev == self.root_identity.dev || self.permits_data(path, dev)
    }

    /// [`allows`](Self::allows) for a child named `name` of an in-scope directory, building the
    /// child path in `scratch` only when the decision depends on it.
    #[inline]
    pub fn allows_child(
        &self,
        parent: &[u8],
        name: &[u8],
        dev: u64,
        is_mount_point: bool,
        scratch: &mut Vec<u8>,
    ) -> bool {
        if !self.is_path_sensitive() {
            return !is_mount_point && dev == self.root_identity.dev;
        }
        join_into(scratch, parent, name);
        self.allows(scratch, dev, is_mount_point)
    }

    /// The path duplicates a logical alias under the physical Data mount and must be skipped
    /// (not even listed as a record).
    pub fn excludes(&self, path: &[u8]) -> bool {
        let Some(boot) = &self.boot else { return false };
        is_within(path, &boot.data_path) && boot.physical_aliases.iter().any(|alias| is_within(path, alias))
    }

    /// An opened directory at `path` has `identity`; may traversal use it? Callers must still
    /// compare the identity with the queued one.
    pub fn allows_identity(&self, path: &[u8], identity: FileIdentity) -> bool {
        if !is_within(path, &self.root_path) || self.excludes(path) {
            return false;
        }
        if path == &self.root_path[..] {
            return identity == self.root_identity;
        }
        if let Some(boot) = &self.boot {
            if path == &boot.data_path[..] {
                return identity == boot.data_identity;
            }
            if let Some((_, expected)) = boot.logical_aliases.iter().find(|(alias, _)| alias == path) {
                return identity == *expected;
            }
        }
        identity.dev == self.root_identity.dev || self.permits_data(path, identity.dev)
    }

    /// A path on the filesystem mounted at `mount` belongs to this mapping.
    pub fn allows_mount(&self, path: &[u8], mount: &str) -> bool {
        if !is_within(path, &self.root_path) || self.excludes(path) {
            return false;
        }
        if self.root_mount.as_deref() == Some(mount) {
            return true;
        }
        let Some(boot) = &self.boot else { return false };
        mount == boot.data_mount
            && (is_within(path, &boot.data_path)
                || boot.logical_aliases.iter().any(|(alias, _)| is_within(path, alias)))
    }

    fn permits_data(&self, path: &[u8], dev: u64) -> bool {
        let Some(boot) = &self.boot else { return false };
        dev == boot.data_identity.dev
            && (is_within(path, &boot.data_path)
                || boot.logical_aliases.iter().any(|(alias, _)| is_within(path, alias)))
    }
}

/// `scratch = parent + "/" + name` without reallocating when capacity suffices.
#[inline]
pub(crate) fn join_into(scratch: &mut Vec<u8>, parent: &[u8], name: &[u8]) {
    scratch.clear();
    scratch.extend_from_slice(parent);
    if parent.last() != Some(&SEP) {
        scratch.push(SEP);
    }
    scratch.extend_from_slice(name);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT_DEV: u64 = 1;
    const DATA_DEV: u64 = 0x1_0000_0002; // wider than 32 bits: device numbers are u64 everywhere
    const OTHER_DEV: u64 = 3;

    fn id(dev: u64, ino: u64) -> FileIdentity {
        FileIdentity { dev, ino }
    }

    fn boot_scope() -> TraversalScope {
        let layout = VolumeLayout {
            root_mount: Some("/".into()),
            boot: Some(BootLayout {
                data_path: b"/System/Volumes/Data".to_vec(),
                data_mount: "/System/Volumes/Data".into(),
                data_identity: id(DATA_DEV, 2),
                logical_aliases: vec![(b"/Users".to_vec(), id(DATA_DEV, 10))],
                physical_aliases: vec![b"/System/Volumes/Data/Users".to_vec()],
            }),
        };
        TraversalScope::new(b"/", id(ROOT_DEV, 2), layout)
    }

    #[test]
    fn boot_volume_data_is_reachable_once() {
        let s = boot_scope();
        // System volume entries.
        assert!(s.allows(b"/Applications", ROOT_DEV, false));
        // Logical alias into the data volume and its contents.
        assert!(s.allows(b"/Users", DATA_DEV, false));
        assert!(s.allows(b"/Users/me/Documents", DATA_DEV, false));
        // The physical Data mount is admitted as a mount point...
        assert!(s.allows(b"/System/Volumes/Data", DATA_DEV, true));
        // ...but its duplicate of /Users is excluded, including sibling-prefix safety.
        assert!(!s.allows(b"/System/Volumes/Data/Users", DATA_DEV, false));
        assert!(s.excludes(b"/System/Volumes/Data/Users/me"));
        assert!(!s.excludes(b"/System/Volumes/Data/Users-old"));
        assert!(s.allows(b"/System/Volumes/Data/Users-old", DATA_DEV, false));
        // Data-device entries outside the data mount or an alias are not admitted.
        assert!(!s.allows(b"/Library", DATA_DEV, false));
        // Other mounts are never crossed.
        assert!(!s.allows(b"/Volumes/External", OTHER_DEV, true));
        assert!(!s.allows(b"/System/Volumes/Data", OTHER_DEV, true));
    }

    #[test]
    fn identities_must_match_aliases_and_root() {
        let s = boot_scope();
        assert!(s.allows_identity(b"/", id(ROOT_DEV, 2)));
        assert!(!s.allows_identity(b"/", id(ROOT_DEV, 3)));
        assert!(s.allows_identity(b"/Users", id(DATA_DEV, 10)));
        assert!(!s.allows_identity(b"/Users", id(DATA_DEV, 11)));
        assert!(s.allows_identity(b"/System/Volumes/Data", id(DATA_DEV, 2)));
        assert!(!s.allows_identity(b"/System/Volumes/Data/Users", id(DATA_DEV, 10)));
        assert!(s.allows_identity(b"/Users/me", id(DATA_DEV, 99)));
        assert!(!s.allows_identity(b"/opt/mounted", id(OTHER_DEV, 5)));
        assert!(s.allows_mount(b"/Users/me", "/System/Volumes/Data"));
        assert!(!s.allows_mount(b"/Library", "/System/Volumes/Data"));
        assert!(s.allows_mount(b"/Library", "/"));
    }

    #[test]
    fn nested_mounts_and_other_devices_stop_traversal() {
        let s = TraversalScope::new(b"/r", id(ROOT_DEV, 5), VolumeLayout { root_mount: Some("/".into()), boot: None });
        let mut scratch = Vec::new();
        assert!(s.allows_child(b"/r", b"a", ROOT_DEV, false, &mut scratch));
        assert!(!s.allows_child(b"/r", b"mnt", OTHER_DEV, true, &mut scratch));
        // A same-device mount point (bind mount, nested APFS snapshot) is still a boundary.
        assert!(!s.allows_child(b"/r", b"bind", ROOT_DEV, true, &mut scratch));
        assert!(!s.allows(b"/elsewhere", ROOT_DEV, false));
        assert!(!s.allows(b"/rx", ROOT_DEV, false));
        assert!(s.allows_child(b"/", b"x", ROOT_DEV, false, &mut scratch));
        join_into(&mut scratch, b"/", b"x");
        assert_eq!(scratch, b"/x");
    }
}
