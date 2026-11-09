//! Per-worker secure directory cursor (Swift `SecureDirectoryCursor`).
//!
//! Opens queued directories component by component from the mapped root with
//! [`Platform::open_child`] (no symlinks, identity checked at every step) and keeps the opened
//! chain (up to [`MAX_CACHED`] descriptors) for the next directory, which in depth-first order
//! usually shares all but the last component. A cached descriptor is never trusted on its own:
//! before reuse, every cached ancestor's current parent/name link is revalidated against the
//! identity the directory was queued with, so a directory renamed or replaced after it was
//! cached cannot attribute its contents to the old path. Paths deeper than the cache are opened
//! from the root without caching.

use crate::mailbox::DirComponent;
use crate::scope::{TraversalScope, join_into};
use orbit_platform::{FileIdentity, Platform, PlatformError};
use std::path::PathBuf;

/// Cached directory descriptors per worker (Swift default 32, at most 64).
pub const MAX_CACHED: usize = 32;

struct Cached<D> {
    name: Box<[u8]>,
    identity: FileIdentity,
    dir: D,
}

pub(crate) enum Opened<'c, D> {
    Cached(&'c D),
    Owned(D),
}

impl<D> Opened<'_, D> {
    pub fn dir(&self) -> &D {
        match self {
            Self::Cached(d) => d,
            Self::Owned(d) => d,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorStats {
    /// Directory opens performed (`open_child`), including revalidation probes.
    pub opens: u64,
    /// Cached ancestors revalidated.
    pub revalidations: u64,
    /// Directories opened from the root because they were deeper than the cache.
    pub uncached: u64,
}

pub(crate) struct Cursor<'a, P: Platform> {
    platform: &'a P,
    scope: &'a TraversalScope,
    root: P::Dir,
    cached: Vec<Cached<P::Dir>>,
    max: usize,
    scratch: Vec<u8>,
    pub stats: CursorStats,
}

fn invalid_component(name: &[u8]) -> bool {
    name.is_empty() || name == b"." || name == b".." || name.contains(&b'/') || name.contains(&0)
}

fn identity_error(path: &[u8]) -> PlatformError {
    PlatformError::IdentityChanged { path: PathBuf::from(String::from_utf8_lossy(path).into_owned()) }
}

impl<'a, P: Platform> Cursor<'a, P> {
    /// Opens the root and checks that it is still the mapped root.
    pub fn new(platform: &'a P, scope: &'a TraversalScope, max: usize) -> Result<Self, PlatformError> {
        let root = open_root_checked(platform, scope)?;
        Ok(Self {
            platform,
            scope,
            root,
            cached: Vec::with_capacity(max.clamp(1, 64)),
            max: max.clamp(1, 64),
            scratch: Vec::new(),
            stats: CursorStats::default(),
        })
    }

    /// The root still names the mapped root directory.
    pub fn validate_root(&self) -> Result<(), PlatformError> {
        open_root_checked(self.platform, self.scope).map(drop)
    }

    fn check_scope(&mut self, depth: usize, components: &[DirComponent], identity: FileIdentity) -> bool {
        if !self.scope.is_path_sensitive() {
            return identity.dev == self.scope.root_identity().dev;
        }
        let mut path = std::mem::take(&mut self.scratch);
        path.clear();
        path.extend_from_slice(self.scope.root_path());
        let mut next = Vec::new();
        for c in &components[..=depth] {
            join_into(&mut next, &path, &c.name);
            std::mem::swap(&mut path, &mut next);
        }
        let allowed = self.scope.allows_identity(&path, identity);
        self.scratch = path;
        allowed
    }

    /// Opens the directory at `components` (root first; empty = the root).
    pub fn open(&mut self, components: &[DirComponent]) -> Result<Opened<'_, P::Dir>, PlatformError> {
        if components.iter().any(|c| invalid_component(&c.name)) {
            self.cached.clear();
            return Err(PlatformError::Unsupported("invalid path component"));
        }
        let result = self.prepare(components);
        if result.is_err() {
            self.cached.clear();
        }
        result?;
        if components.len() >= self.max {
            return self.open_uncached(components).map(Opened::Owned);
        }
        Ok(Opened::Cached(self.cached.last().map_or(&self.root, |c| &c.dir)))
    }

    fn prepare(&mut self, components: &[DirComponent]) -> Result<(), PlatformError> {
        if components.len() >= self.max {
            return Ok(());
        }
        let mut prefix = 0;
        while prefix < self.cached.len().min(components.len())
            && self.cached[prefix].identity == components[prefix].identity
            && *self.cached[prefix].name == *components[prefix].name
        {
            prefix += 1;
        }
        self.cached.truncate(prefix);
        // Revalidate every cached ancestor's link from its (cached) parent.
        for depth in 0..prefix {
            let component = &components[depth];
            let parent = if depth == 0 { &self.root } else { &self.cached[depth - 1].dir };
            self.stats.opens += 1;
            self.stats.revalidations += 1;
            if self.platform.child_identity(parent, &component.name)? != component.identity {
                return Err(identity_error(&component.name));
            }
            if !self.check_scope(depth, components, component.identity) {
                return Err(identity_error(&component.name));
            }
        }
        for depth in prefix..components.len() {
            let component = &components[depth];
            let parent = self.cached.last().map_or(&self.root, |c| &c.dir);
            self.stats.opens += 1;
            let dir = self.platform.open_child(parent, &component.name, component.identity)?;
            if !self.check_scope(depth, components, component.identity) {
                return Err(identity_error(&component.name));
            }
            self.cached.push(Cached { name: component.name.clone(), identity: component.identity, dir });
        }
        Ok(())
    }

    fn open_uncached(&mut self, components: &[DirComponent]) -> Result<P::Dir, PlatformError> {
        self.stats.uncached += 1;
        let mut current: Option<P::Dir> = None;
        for (depth, component) in components.iter().enumerate() {
            let parent = current.as_ref().unwrap_or(&self.root);
            self.stats.opens += 1;
            let next = self.platform.open_child(parent, &component.name, component.identity)?;
            if !self.check_scope(depth, components, component.identity) {
                return Err(identity_error(&component.name));
            }
            current = Some(next);
        }
        Ok(current.expect("uncached paths have components"))
    }
}

fn open_root_checked<P: Platform>(platform: &P, scope: &TraversalScope) -> Result<P::Dir, PlatformError> {
    let path = crate::discovery::native_path(scope.root_path());
    let (dir, identity) = platform.open_root(&path)?;
    if identity != scope.root_identity() {
        return Err(PlatformError::IdentityChanged { path });
    }
    Ok(dir)
}
