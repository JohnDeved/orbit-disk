//! Merges unacknowledged mailbox batches into a stage read. Ported from
//! `PendingInventoryRead.swift`.
//!
//! The reader captures a [`MailboxSnapshot`] *before* its SQLite read transaction, reads the
//! durable watermark inside the transaction and keeps only batches above it. Folder totals then
//! add the pending records to every ancestor, and hard-link ownership is recomputed across the
//! memory/database boundary: for every multi-link identity in the snapshot, the path that sorts
//! first by [`crate::hardlinks`] — pending or durable — owns the allocation.

use crate::hardlinks as link_order;
use crate::mailbox::{ListingBatch, MailboxSnapshot};
use crate::schema;
use crate::stage::writer::Traversal;
use crate::store::{Delta, Row, apply_delta};
use crate::types::ScanState;
use crate::view::{NodeFlags, NodeMeta};
use crate::{CoreError, Result};
use orbit_platform::{Cancellation, EntryKind, EntryRecord, FileIdentity};
use std::collections::{HashMap, HashSet};

/// A durable row with the same identity as a pending multi-link file.
#[derive(Clone, Debug)]
pub struct DurableLink {
    pub path: Vec<u8>,
    pub allocated: u64,
    pub raw_allocated: u64,
}

pub struct PendingRead<'a> {
    snapshot: &'a MailboxSnapshot,
    by_parent: HashMap<&'a [u8], Vec<&'a ListingBatch>>,
    completed: HashSet<&'a [u8]>,
    deltas: HashMap<Vec<u8>, Delta>,
    affected_parents: HashSet<Vec<u8>>,
    allocations: HashMap<Vec<u8>, u64>,
    has_allocation_changes: bool,
    root: &'a [u8],
    root_dev: u64,
    traversal: Traversal,
}

impl std::fmt::Debug for PendingRead<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingRead").field("parents", &self.by_parent.len()).finish_non_exhaustive()
    }
}

fn parent_of(path: &[u8]) -> &[u8] {
    crate::paths::parent(path).unwrap_or(path)
}

impl<'a> PendingRead<'a> {
    /// `watermark` is the durable sequence read inside the reader's transaction (0 when the
    /// database holds another generation). `durable_links` returns the durable rows of one
    /// identity from the same transaction.
    pub fn new(
        snapshot: &'a MailboxSnapshot,
        watermark: u64,
        traversal: Traversal,
        mut durable_links: impl FnMut(FileIdentity) -> Result<Vec<DurableLink>>,
        cancel: Option<&Cancellation>,
    ) -> Result<Self> {
        if !snapshot.is_current() {
            return Err(CoreError::Cancelled);
        }
        let root: &'a [u8] = &snapshot.root_path;
        let mut read = Self {
            snapshot,
            by_parent: HashMap::new(),
            completed: HashSet::new(),
            deltas: HashMap::new(),
            affected_parents: HashSet::new(),
            allocations: HashMap::new(),
            has_allocation_changes: false,
            root,
            root_dev: snapshot.root_identity.dev,
            traversal,
        };
        let mut links: HashMap<FileIdentity, Vec<(Vec<u8>, u64)>> = HashMap::new();
        for (offset, batch) in snapshot.batches_after(snapshot.committed).enumerate() {
            if offset & 63 == 0 {
                schema::check(cancel)?;
            }
            for meta in batch.records.metas() {
                if meta.kind != EntryKind::Directory && meta.nlink > 1 {
                    links.entry(meta.id).or_default();
                }
            }
        }
        for (offset, batch) in snapshot.batches_after(watermark).enumerate() {
            if offset & 63 == 0 {
                schema::check(cancel)?;
            }
            if batch.failure.is_some() {
                continue;
            }
            let batch: &'a ListingBatch = batch;
            read.by_parent.entry(&batch.parent_path).or_default().push(batch);
            if batch.completion.is_some() {
                read.completed.insert(&batch.parent_path);
            }
            read.adjust(
                &batch.parent_path,
                Delta {
                    size: batch.totals.logical as i64,
                    allocated: batch.totals.allocated as i64,
                    count: batch.totals.files as i64,
                },
            );
            for record in batch.records.iter() {
                if !record.is_dir() && record.nlink > 1 {
                    let path = crate::paths::join(&batch.parent_path, record.name);
                    links.entry(record.id).or_default().push((path, record.allocated));
                }
            }
        }
        for (identity, pending) in links {
            schema::check(cancel)?;
            let durable = durable_links(identity)?;
            let mut winner: Option<&[u8]> = None;
            for path in pending.iter().map(|(p, _)| p.as_slice()).chain(durable.iter().map(|d| d.path.as_slice())) {
                if winner.is_none_or(|w| link_order::precedes(path, w)) {
                    winner = Some(path);
                }
            }
            let winner = winner.map(<[u8]>::to_vec);
            for (path, allocated) in &pending {
                let charge = if winner.as_deref() == Some(path.as_slice()) { *allocated } else { 0 };
                read.allocations.insert(path.clone(), charge);
                read.adjust(
                    parent_of(path),
                    Delta { allocated: charge as i64 - *allocated as i64, ..Delta::default() },
                );
            }
            for link in &durable {
                let charge = if winner.as_deref() == Some(link.path.as_slice()) { link.raw_allocated } else { 0 };
                read.allocations.insert(link.path.clone(), charge);
                if charge != link.allocated {
                    read.has_allocation_changes = true;
                }
                read.adjust(
                    parent_of(&link.path),
                    Delta { allocated: charge as i64 - link.allocated as i64, ..Delta::default() },
                );
            }
        }
        read.validate()?;
        Ok(read)
    }

    /// Nothing pending changes the durable rows.
    pub fn is_empty(&self) -> bool {
        self.by_parent.is_empty() && !self.has_allocation_changes
    }

    /// The captured mailbox still exists and was not cancelled.
    pub fn validate(&self) -> Result<()> {
        if self.snapshot.is_current() { Ok(()) } else { Err(CoreError::Cancelled) }
    }

    fn adjust(&mut self, parent: &[u8], delta: Delta) {
        if delta.size == 0 && delta.allocated == 0 && delta.count == 0 {
            return;
        }
        let mut current = parent;
        while crate::paths::is_within(current, self.root) {
            let entry = self.deltas.entry(current.to_vec()).or_default();
            entry.size += delta.size;
            entry.allocated += delta.allocated;
            entry.count += delta.count;
            self.affected_parents.insert(parent_of(current).to_vec());
            if current == self.root {
                break;
            }
            match crate::paths::parent(current) {
                Some(next) => current = next,
                None => break,
            }
        }
    }

    /// Pending batches add, replace or resize children of `parent`.
    pub fn changes_children_order(&self, parent: &[u8]) -> bool {
        self.by_parent.contains_key(parent) || self.affected_parents.contains(parent)
    }

    fn record(&self, path: &[u8]) -> Option<EntryRecord<'a>> {
        let batches = self.by_parent.get(parent_of(path))?;
        let name = crate::paths::file_name(path);
        batches
            .iter()
            .rev()
            .find_map(|batch| (0..batch.records.len()).rev().map(|i| batch.records.get(i)).find(|r| r.name == name))
    }

    /// The row at `path` with pending records and totals applied.
    pub(crate) fn node(&self, path: &[u8], durable: Option<Row>) -> Option<Row> {
        let record = self.record(path);
        self.merge(path, record, durable)
    }

    fn merge(&self, path: &[u8], record: Option<EntryRecord<'_>>, durable: Option<Row>) -> Option<Row> {
        let is_dir = match (&record, &durable) {
            (Some(record), _) => record.is_dir(),
            (None, Some(row)) => row.meta.flags.is_dir,
            (None, None) => return None,
        };
        let delta = if is_dir { self.deltas.get(path).copied().unwrap_or_default() } else { Delta::default() };
        let mut state = durable.as_ref().map_or(ScanState::Complete, |r| r.meta.state);
        let mut accessible = durable.as_ref().is_none_or(|r| r.meta.flags.accessible);
        if let Some(record) = &record {
            accessible = !is_dir || self.traversal.allows(path, record.id.dev, record.is_mount_point, self.root_dev);
            state = if !is_dir || !accessible { ScanState::Complete } else { ScanState::Pending };
        }
        if is_dir && self.by_parent.contains_key(path) {
            state = if self.completed.contains(path) { ScanState::Complete } else { ScanState::Scanning };
        } else if !is_dir && self.by_parent.contains_key(parent_of(path)) {
            state = if self.completed.contains(parent_of(path)) { ScanState::Complete } else { ScanState::Pending };
        }
        // A pending directory record that replaces a durable directory keeps the durable totals
        // (committed descendants); pending descendants add their deltas on top.
        let durable_dir = durable.as_ref().filter(|r| r.meta.flags.is_dir && is_dir).map(|r| (r.id, r.meta));
        let mut row = match record {
            Some(record) => Row {
                id: durable_dir.map_or(0, |(id, _)| id),
                name: record.name.to_vec(),
                meta: NodeMeta {
                    flags: NodeFlags {
                        is_dir,
                        is_symlink: record.kind == EntryKind::Symlink,
                        is_mount_point: record.is_mount_point,
                        accessible,
                    },
                    size: match durable_dir {
                        Some((_, meta)) => meta.size,
                        None if is_dir => 0,
                        None => record.size,
                    },
                    allocated: match durable_dir {
                        Some((_, meta)) => meta.allocated,
                        None => {
                            self.allocations.get(path).copied().unwrap_or(if is_dir { 0 } else { record.allocated })
                        }
                    },
                    raw_allocated: if is_dir { 0 } else { record.allocated },
                    file_count: match durable_dir {
                        Some((_, meta)) => meta.file_count,
                        None => u64::from(!is_dir),
                    },
                    identity: Some(record.id),
                    state,
                },
            },
            None => {
                let mut row = durable?;
                if let Some(allocated) = self.allocations.get(path) {
                    row.meta.allocated = *allocated;
                }
                row.meta.flags.accessible = accessible;
                row.meta.state = state;
                row
            }
        };
        apply_delta(&mut row.meta, delta, 1);
        Some(row)
    }

    /// Merges durable children of `parent` with its pending records, largest first.
    pub(crate) fn children(
        &self,
        parent: &[u8],
        durable: Vec<Row>,
        limit: Option<usize>,
        cancel: Option<&Cancellation>,
    ) -> Result<Vec<Row>> {
        let pending = self.by_parent.get(parent).map(Vec::as_slice).unwrap_or(&[]);
        let mut by_name: HashMap<Vec<u8>, (Option<Row>, Option<EntryRecord<'a>>)> =
            HashMap::with_capacity(durable.len());
        for row in durable {
            by_name.insert(row.name.clone(), (Some(row), None));
        }
        for batch in pending {
            schema::check(cancel)?;
            for record in batch.records.iter() {
                by_name.entry(record.name.to_vec()).or_default().1 = Some(record);
            }
        }
        let mut rows = Vec::with_capacity(by_name.len());
        let mut path = Vec::new();
        for (index, (name, (durable, record))) in by_name.into_iter().enumerate() {
            if index & 255 == 0 {
                schema::check(cancel)?;
            }
            crate::stage::writer::join_into(&mut path, parent, &name);
            if let Some(row) = self.merge(&path, record, durable) {
                rows.push(row);
            }
        }
        let order = |a: &Row, b: &Row| b.meta.size.cmp(&a.meta.size).then_with(|| a.name.cmp(&b.name));
        if let Some(limit) = limit
            && limit < rows.len()
        {
            if limit == 0 {
                return Ok(Vec::new());
            }
            rows.select_nth_unstable_by(limit - 1, order);
            rows.truncate(limit);
        }
        rows.sort_unstable_by(order);
        schema::check(cancel)?;
        Ok(rows)
    }
}
