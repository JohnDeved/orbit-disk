//! Owned, allocation-light storage for listing records. One `RecordBatch` holds up to a few
//! thousand entries with all names in a single byte arena (no per-entry heap allocation).

use orbit_platform::{EntryKind, EntryRecord, FileIdentity};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordMeta {
    name_start: u32,
    name_len: u32,
    pub kind: EntryKind,
    pub is_mount_point: bool,
    pub nlink: u32,
    pub id: FileIdentity,
    pub size: u64,
    pub allocated: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecordBatch {
    names: Vec<u8>,
    metas: Vec<RecordMeta>,
}

impl RecordBatch {
    pub fn with_capacity(records: usize, name_bytes: usize) -> Self {
        Self { names: Vec::with_capacity(name_bytes), metas: Vec::with_capacity(records) }
    }

    pub fn push(&mut self, record: &EntryRecord<'_>) {
        let start = u32::try_from(self.names.len()).expect("record batch name arena exceeds 4 GiB");
        self.names.extend_from_slice(record.name);
        self.metas.push(RecordMeta {
            name_start: start,
            name_len: record.name.len() as u32,
            kind: record.kind,
            is_mount_point: record.is_mount_point,
            nlink: record.nlink,
            id: record.id,
            size: record.size,
            allocated: record.allocated,
        });
    }

    pub fn extend(&mut self, records: &[EntryRecord<'_>]) {
        self.metas.reserve(records.len());
        for record in records {
            self.push(record);
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.metas.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.metas.is_empty()
    }

    #[inline]
    pub fn get(&self, index: usize) -> EntryRecord<'_> {
        let meta = &self.metas[index];
        self.view(meta)
    }

    #[inline]
    pub fn name(&self, index: usize) -> &[u8] {
        let meta = &self.metas[index];
        &self.names[meta.name_start as usize..(meta.name_start + meta.name_len) as usize]
    }

    pub fn metas(&self) -> &[RecordMeta] {
        &self.metas
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = EntryRecord<'_>> + '_ {
        self.metas.iter().map(|meta| self.view(meta))
    }

    /// Approximate heap footprint, used for mailbox back-pressure accounting.
    pub fn byte_count(&self) -> usize {
        self.names.capacity() + self.metas.capacity() * std::mem::size_of::<RecordMeta>()
    }

    /// Drops spare capacity so pending-byte accounting matches the data held.
    pub fn shrink_to_fit(&mut self) {
        self.names.shrink_to_fit();
        self.metas.shrink_to_fit();
    }

    /// Keeps the first `len` records.
    pub fn truncate(&mut self, len: usize) {
        if len < self.metas.len() {
            let end = self.metas[len].name_start as usize;
            self.metas.truncate(len);
            self.names.truncate(end);
        }
    }

    pub fn clear(&mut self) {
        self.names.clear();
        self.metas.clear();
    }

    #[inline]
    fn view(&self, meta: &RecordMeta) -> EntryRecord<'_> {
        EntryRecord {
            name: &self.names[meta.name_start as usize..(meta.name_start + meta.name_len) as usize],
            kind: meta.kind,
            id: meta.id,
            nlink: meta.nlink,
            size: meta.size,
            allocated: meta.allocated,
            is_mount_point: meta.is_mount_point,
        }
    }

    /// Logical bytes, allocated bytes and file count of the batch's non-directory entries.
    /// Hard-link deduplication is *not* applied here; that happens in the inventory.
    pub fn totals(&self) -> BatchTotals {
        let mut totals = BatchTotals::default();
        for meta in &self.metas {
            if meta.kind != EntryKind::Directory {
                totals.logical += meta.size;
                totals.allocated += meta.allocated;
                totals.files += 1;
            }
        }
        totals
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BatchTotals {
    pub logical: u64,
    pub allocated: u64,
    pub files: u64,
}
