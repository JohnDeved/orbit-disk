//! Cooperative locks and identity fences around stage and saved-map files.
//!
//! Ported from `StageLease.swift`, `RootMutationLease.swift` and `SnapshotExpectation.swift`.
//!
//! - [`StageLease`]: exclusive, non-blocking `flock` on `<staging>/.orbit-stages/<key>.lock`
//!   plus held descriptors on the stage directory and database. Every check compares the paths
//!   with the held descriptors (device, inode, owner, mode `0600`/`0700`, `nlink == 1`), so a
//!   swapped file or symlink is detected instead of followed. Hold it until every SQLite
//!   connection to the stage is closed.
//! - [`RootMutationLease`]: the per-root lock every saved-map writer (publication, invalidation,
//!   recovery) holds, in `<locks>/.orbit-locks/<sha256(root)>.lock`. It also stores the durable
//!   *invalidation generation* marker that makes stale stages unpublishable.
//! - [`SnapshotExpectation`]: which saved map a stage expects to replace; checked again under
//!   the mutation lease right before publication.
//!
//! The file primitives are POSIX (`openat`, `flock`, `fstatat`) and live in [`sys`]; a Windows
//! port needs equivalents (see the module report).

use crate::{CoreError, Result};
use orbit_platform::{Cancellation, FileIdentity};
use std::path::{Path, PathBuf};

const STAGES_DIR: &str = ".orbit-stages";
const LOCKS_DIR: &str = ".orbit-locks";

fn unsafe_file(what: &str) -> CoreError {
    CoreError::Invalid(format!("unsafe {what}"))
}

/// Stage file key: SHA-256 over the root path, its identity and the inventory format, so a
/// replaced root or a format change never resumes an old stage.
pub fn stage_key(root: &[u8], identity: FileIdentity) -> String {
    use sha2::Digest;
    let mut bytes = Vec::with_capacity(root.len() + 64);
    bytes.extend_from_slice(root);
    bytes.push(0);
    bytes.extend_from_slice(format!("{}:{}", identity.dev, identity.ino).as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(format!("inventory:{}", crate::schema::VERSION).as_bytes());
    crate::schema::hex(&sha2::Sha256::digest(&bytes))
}

/// Path of the stage database for `root` inside `staging` (whether or not it exists).
pub fn stage_database_path(staging: &Path, root: &[u8], identity: FileIdentity) -> PathBuf {
    staging.join(STAGES_DIR).join(format!("{}.sqlite", stage_key(root, identity)))
}

/// Swift `ResumableStage.hasCandidate`: a stage file exists for this root and identity.
pub fn has_stage(staging: &Path, root: &[u8], identity: FileIdentity) -> bool {
    std::fs::symlink_metadata(stage_database_path(staging, root, identity)).is_ok()
}

#[cfg(unix)]
pub use unix::{RootMutationLease, StageLease};

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::fd::{AsFd, OwnedFd};
    use sys::Stat;

    pub struct StageLease {
        dir: OwnedFd,
        lock: OwnedFd,
        db: OwnedFd,
        directory_path: PathBuf,
        name: String,
        lock_name: String,
        expected_dir: Stat,
        expected_lock: Stat,
        expected_db: Stat,
        database: PathBuf,
    }

    impl std::fmt::Debug for StageLease {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("StageLease").field("database", &self.database).finish_non_exhaustive()
        }
    }

    fn private_file(info: &Stat) -> bool {
        info.is_regular() && info.uid == sys::euid() && info.permissions() == 0o600 && info.nlink == 1
    }

    fn private_dir(info: &Stat) -> bool {
        info.is_dir() && info.uid == sys::euid() && info.permissions() == 0o700
    }

    impl StageLease {
        /// Takes the stage lock for `root` without blocking. Fails with `CoreError::InUse` when
        /// another session holds it.
        pub fn acquire(staging: &Path, root: &[u8], root_identity: FileIdentity) -> Result<Self> {
            let base = sys::open_dir(staging).map_err(|_| unsafe_file("staging directory"))?;
            sys::mkdirat(base.as_fd(), STAGES_DIR, 0o700).map_err(|_| unsafe_file("create stage directory"))?;
            let dir = sys::openat_dir(base.as_fd(), STAGES_DIR).map_err(|_| unsafe_file("open stage directory"))?;
            let expected_dir = sys::fstat(dir.as_fd())?;
            if !private_dir(&expected_dir) {
                return Err(unsafe_file("stage directory permissions"));
            }
            let key = stage_key(root, root_identity);
            let lock_name = format!("{key}.lock");
            let name = format!("{key}.sqlite");
            let lock = sys::openat_file(dir.as_fd(), &lock_name, true).map_err(|_| unsafe_file("open lock"))?;
            let expected_lock = sys::fstat(lock.as_fd())?;
            if !private_file(&expected_lock) {
                return Err(unsafe_file("lock identity"));
            }
            let directory_path = staging.join(STAGES_DIR);
            if !orbit_platform::posix::try_flock(
                lock.as_fd(),
                orbit_platform::posix::LockMode::Exclusive,
                &directory_path,
            )? {
                return Err(CoreError::InUse(String::from_utf8_lossy(root).into_owned()));
            }
            let db = sys::openat_file(dir.as_fd(), &name, true).map_err(|_| unsafe_file("open database"))?;
            let expected_db = sys::fstat(db.as_fd())?;
            if !private_file(&expected_db) {
                return Err(unsafe_file("database identity"));
            }
            let lease = Self {
                dir,
                lock,
                db,
                database: directory_path.join(&name),
                directory_path,
                name,
                lock_name,
                expected_dir,
                expected_lock,
                expected_db,
            };
            lease.validate_paths()?;
            if expected_db.size != 0 {
                crate::schema::validate_stage_file(&lease.database)?;
            }
            lease.validate_paths()?;
            Ok(lease)
        }

        pub fn database_path(&self) -> &Path {
            &self.database
        }

        /// The stage directory, lock and database are still the files this lease opened.
        pub fn validate_paths(&self) -> Result<()> {
            let replaced = || unsafe_file("stage: replaced");
            let actual_dir = sys::lstat(&self.directory_path)?.ok_or_else(replaced)?;
            let held_dir = sys::fstat(self.dir.as_fd())?;
            let lock = sys::fstatat(self.dir.as_fd(), &self.lock_name)?.ok_or_else(replaced)?;
            let held_lock = sys::fstat(self.lock.as_fd())?;
            let db = sys::fstatat(self.dir.as_fd(), &self.name)?.ok_or_else(replaced)?;
            let held_db = sys::fstat(self.db.as_fd())?;
            let ok = private_dir(&actual_dir)
                && actual_dir.same_file(&self.expected_dir)
                && held_dir.same_file(&self.expected_dir)
                && private_file(&lock)
                && lock.same_file(&self.expected_lock)
                && held_lock.same_file(&self.expected_lock)
                && private_file(&db)
                && db.same_file(&self.expected_db)
                && held_db.same_file(&self.expected_db);
            if !ok {
                return Err(replaced());
            }
            for suffix in ["-wal", "-shm", "-journal"] {
                if let Some(info) = sys::fstatat(self.dir.as_fd(), &format!("{}{suffix}", self.name))?
                    && !private_file(&info)
                {
                    return Err(unsafe_file("stage sidecar"));
                }
            }
            Ok(())
        }

        /// Deletes the stage database and its sidecars (after validating each is private).
        pub fn remove_stage_files(&self) -> Result<()> {
            self.validate_paths()?;
            for suffix in ["-wal", "-shm", "-journal", ""] {
                let file = format!("{}{suffix}", self.name);
                let Some(info) = sys::fstatat(self.dir.as_fd(), &file)? else { continue };
                if !private_file(&info) || (suffix.is_empty() && !info.same_file(&self.expected_db)) {
                    return Err(unsafe_file("stage cleanup"));
                }
                sys::unlinkat(self.dir.as_fd(), &file)?;
            }
            Ok(())
        }
    }

    pub struct RootMutationLease {
        dir: OwnedFd,
        lock: OwnedFd,
        lock_name: String,
        directory_path: PathBuf,
        expected_dir: Stat,
        expected_lock: Stat,
    }

    impl std::fmt::Debug for RootMutationLease {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("RootMutationLease").field("lock", &self.lock_name).finish_non_exhaustive()
        }
    }

    impl RootMutationLease {
        /// Blocks (polling every 10 ms) until the lock is free or `cancel` fires.
        pub fn acquire(locks: &Path, root: &[u8], cancel: &Cancellation) -> Result<Self> {
            loop {
                if let Some(lease) = Self::try_acquire(locks, root)? {
                    return Ok(lease);
                }
                if cancel.is_cancelled() {
                    return Err(CoreError::Cancelled);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }

        /// Takes the lock if it is free; `Ok(None)` when another holder has it.
        pub fn try_acquire(locks: &Path, root: &[u8]) -> Result<Option<Self>> {
            let parent = sys::open_dir(locks).map_err(|_| unsafe_file("lock parent directory"))?;
            sys::mkdirat(parent.as_fd(), LOCKS_DIR, 0o700).map_err(|_| unsafe_file("create lock directory"))?;
            let dir = sys::openat_dir(parent.as_fd(), LOCKS_DIR).map_err(|_| unsafe_file("open lock directory"))?;
            let expected_dir = sys::fstat(dir.as_fd())?;
            if !private_dir(&expected_dir) {
                return Err(unsafe_file("lock directory permissions"));
            }
            let lock_name = format!("{}.lock", crate::schema::root_digest(root));
            let lock = sys::openat_file(dir.as_fd(), &lock_name, true).map_err(|_| unsafe_file("open root lock"))?;
            let expected_lock = sys::fstat(lock.as_fd())?;
            if !private_file(&expected_lock) {
                return Err(unsafe_file("root lock identity"));
            }
            let directory_path = locks.join(LOCKS_DIR);
            if !orbit_platform::posix::try_flock(
                lock.as_fd(),
                orbit_platform::posix::LockMode::Exclusive,
                &directory_path,
            )? {
                return Ok(None);
            }
            let lease = Self { dir, lock, lock_name, directory_path, expected_dir, expected_lock };
            lease.validate()?;
            Ok(Some(lease))
        }

        pub fn validate(&self) -> Result<()> {
            let replaced = || unsafe_file("root lock: replaced");
            let dir = sys::lstat(&self.directory_path)?.ok_or_else(replaced)?;
            let file = sys::fstatat(self.dir.as_fd(), &self.lock_name)?.ok_or_else(replaced)?;
            let held = sys::fstat(self.lock.as_fd())?;
            if !(private_dir(&dir)
                && dir.same_file(&self.expected_dir)
                && private_file(&file)
                && file.same_file(&self.expected_lock)
                && held.same_file(&self.expected_lock))
            {
                return Err(replaced());
            }
            Ok(())
        }

        /// The durable invalidation marker (`"0"` before the first invalidation).
        pub fn invalidation_generation(&self) -> Result<String> {
            self.validate()?;
            let name = format!("{}.generation", self.lock_name);
            let marker = match sys::openat_file(self.dir.as_fd(), &name, false) {
                Ok(fd) => fd,
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok("0".into()),
                Err(_) => return Err(unsafe_file("invalidation generation")),
            };
            let held = sys::fstat(marker.as_fd())?;
            let path = sys::fstatat(self.dir.as_fd(), &name)?;
            if !private_file(&held) || !path.is_some_and(|p| p.same_file(&held)) {
                return Err(unsafe_file("invalidation generation identity"));
            }
            let mut buffer = [0u8; 64];
            let count = sys::read(marker.as_fd(), &mut buffer)?;
            let text = std::str::from_utf8(&buffer[..count]).ok().filter(|t| uuid::Uuid::parse_str(t).is_ok());
            match text {
                Some(text) if count > 0 && count < buffer.len() => Ok(text.to_string()),
                _ => Err(unsafe_file("invalidation generation contents")),
            }
        }

        /// Durably replaces the invalidation marker with a new random generation.
        pub fn invalidate_generation(&self) -> Result<()> {
            self.invalidation_generation()?;
            let name = format!("{}.generation", self.lock_name);
            let temporary = format!(".generation-{}", uuid::Uuid::new_v4().hyphenated());
            let marker = sys::create_exclusive(self.dir.as_fd(), &temporary)
                .map_err(|_| unsafe_file("create invalidation generation"))?;
            let result = (|| {
                let generation = uuid::Uuid::new_v4().hyphenated().to_string().to_uppercase();
                sys::write_all(marker.as_fd(), generation.as_bytes())?;
                sys::fsync(marker.as_fd())?;
                self.validate()?;
                sys::renameat(self.dir.as_fd(), &temporary, &name)?;
                sys::fsync(self.dir.as_fd())?;
                Ok(())
            })();
            let _ = sys::unlinkat(self.dir.as_fd(), &temporary);
            result
        }
    }

    pub(crate) mod sys {
        use crate::{CoreError, Result};
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
        use std::os::unix::ffi::OsStrExt;
        use std::path::Path;

        const DIR_FLAGS: libc::c_int = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct Stat {
            pub dev: u64,
            pub ino: u64,
            pub mode: u32,
            pub uid: u32,
            pub nlink: u64,
            pub size: i64,
            pub mtime: (i64, i64),
            pub ctime: (i64, i64),
        }

        impl Stat {
            pub fn is_regular(&self) -> bool {
                self.mode & libc::S_IFMT as u32 == libc::S_IFREG as u32
            }
            pub fn is_dir(&self) -> bool {
                self.mode & libc::S_IFMT as u32 == libc::S_IFDIR as u32
            }
            pub fn permissions(&self) -> u32 {
                self.mode & 0o777
            }
            pub fn same_file(&self, other: &Stat) -> bool {
                self.dev == other.dev && self.ino == other.ino
            }
        }

        #[allow(clippy::unnecessary_cast)]
        fn convert(info: &libc::stat) -> Stat {
            let id = orbit_platform::posix::stat_identity(info);
            Stat {
                dev: id.dev,
                ino: id.ino,
                mode: info.st_mode as u32,
                uid: info.st_uid as u32,
                nlink: info.st_nlink as u64,
                size: info.st_size as i64,
                mtime: (info.st_mtime as i64, info.st_mtime_nsec as i64),
                ctime: (info.st_ctime as i64, info.st_ctime_nsec as i64),
            }
        }

        fn c(name: &[u8]) -> std::io::Result<CString> {
            CString::new(name).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
        }

        fn fd_result(fd: libc::c_int) -> std::io::Result<OwnedFd> {
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: the caller passes a descriptor freshly returned by open/openat.
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }

        pub fn euid() -> u32 {
            // SAFETY: geteuid has no preconditions.
            unsafe { libc::geteuid() }
        }

        pub fn open_dir(path: &Path) -> std::io::Result<OwnedFd> {
            let path = c(path.as_os_str().as_bytes())?;
            // SAFETY: NUL-terminated path.
            fd_result(unsafe { libc::open(path.as_ptr(), DIR_FLAGS) })
        }

        pub fn mkdirat(dir: BorrowedFd<'_>, name: &str, mode: libc::mode_t) -> std::io::Result<()> {
            let name = c(name.as_bytes())?;
            // SAFETY: live descriptor and NUL-terminated name.
            if unsafe { libc::mkdirat(dir.as_raw_fd(), name.as_ptr(), mode) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EEXIST) {
                    return Err(error);
                }
            }
            Ok(())
        }

        pub fn openat_dir(dir: BorrowedFd<'_>, name: &str) -> std::io::Result<OwnedFd> {
            let name = c(name.as_bytes())?;
            // SAFETY: live descriptor and NUL-terminated name.
            fd_result(unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), DIR_FLAGS) })
        }

        /// Opens (optionally creating, mode 0600) a regular file without following symlinks.
        pub fn openat_file(dir: BorrowedFd<'_>, name: &str, create: bool) -> std::io::Result<OwnedFd> {
            let name = c(name.as_bytes())?;
            let flags =
                libc::O_NOFOLLOW | libc::O_CLOEXEC | if create { libc::O_CREAT | libc::O_RDWR } else { libc::O_RDONLY };
            // SAFETY: live descriptor and NUL-terminated name; mode is passed as the variadic arg.
            fd_result(unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, 0o600 as libc::c_uint) })
        }

        pub fn create_exclusive(dir: BorrowedFd<'_>, name: &str) -> std::io::Result<OwnedFd> {
            let name = c(name.as_bytes())?;
            let flags = libc::O_CREAT | libc::O_EXCL | libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
            // SAFETY: live descriptor and NUL-terminated name.
            fd_result(unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags, 0o600 as libc::c_uint) })
        }

        pub fn fstat(fd: BorrowedFd<'_>) -> Result<Stat> {
            // SAFETY: zeroed stat is valid POD.
            let mut info: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: live descriptor, writable out-param.
            if unsafe { libc::fstat(fd.as_raw_fd(), &mut info) } != 0 {
                return Err(CoreError::Io(std::io::Error::last_os_error()));
            }
            Ok(convert(&info))
        }

        /// `fstatat(AT_SYMLINK_NOFOLLOW)`; `None` when the name does not exist.
        pub fn fstatat(dir: BorrowedFd<'_>, name: &str) -> Result<Option<Stat>> {
            let name = c(name.as_bytes())?;
            // SAFETY: zeroed stat is valid POD.
            let mut info: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: live descriptor, NUL-terminated name, writable out-param.
            if unsafe { libc::fstatat(dir.as_raw_fd(), name.as_ptr(), &mut info, libc::AT_SYMLINK_NOFOLLOW) } != 0 {
                let error = std::io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ENOENT) { Ok(None) } else { Err(error.into()) };
            }
            Ok(Some(convert(&info)))
        }

        /// `lstat`; `None` when the path does not exist.
        pub fn lstat(path: &Path) -> Result<Option<Stat>> {
            let path = c(path.as_os_str().as_bytes())?;
            // SAFETY: zeroed stat is valid POD.
            let mut info: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: NUL-terminated path, writable out-param.
            if unsafe { libc::lstat(path.as_ptr(), &mut info) } != 0 {
                let error = std::io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ENOENT) { Ok(None) } else { Err(error.into()) };
            }
            Ok(Some(convert(&info)))
        }

        pub fn unlinkat(dir: BorrowedFd<'_>, name: &str) -> Result<()> {
            let name = c(name.as_bytes())?;
            // SAFETY: live descriptor and NUL-terminated name.
            if unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }

        pub fn renameat(dir: BorrowedFd<'_>, from: &str, to: &str) -> Result<()> {
            let (from, to) = (c(from.as_bytes())?, c(to.as_bytes())?);
            // SAFETY: live descriptor and NUL-terminated names.
            if unsafe { libc::renameat(dir.as_raw_fd(), from.as_ptr(), dir.as_raw_fd(), to.as_ptr()) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }

        pub fn fsync(fd: BorrowedFd<'_>) -> Result<()> {
            // SAFETY: live descriptor.
            if unsafe { libc::fsync(fd.as_raw_fd()) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(())
        }

        pub fn read(fd: BorrowedFd<'_>, buffer: &mut [u8]) -> Result<usize> {
            // SAFETY: live descriptor; the buffer is writable for its full length.
            let count = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if count < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok(count as usize)
        }

        pub fn write_all(fd: BorrowedFd<'_>, mut bytes: &[u8]) -> Result<()> {
            while !bytes.is_empty() {
                // SAFETY: live descriptor; `bytes` is readable for its full length.
                let count = unsafe { libc::write(fd.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
                if count <= 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                bytes = &bytes[count as usize..];
            }
            Ok(())
        }
    }
}

#[cfg(unix)]
pub(crate) use unix::sys;

/// Creates `path` (and parents) with mode `0700` and checks the result is a directory we own.
pub fn create_private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        let info = sys::lstat(path)?.ok_or_else(|| unsafe_file("directory"))?;
        if !info.is_dir() || info.uid != sys::euid() {
            return Err(CoreError::Invalid("The index directory is not a safe folder.".into()));
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
    Ok(())
}

/// Which saved map a stage expects to replace. Swift: `SnapshotExpectation`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SnapshotExpectation {
    /// No saved map existed.
    Absent,
    /// The saved map with this snapshot id.
    Matching(String),
    /// An unreadable owned file, fingerprinted (dev, inode, size, mtime, ctime) so that its
    /// replacement or mutation is detected while mapping proceeds.
    Invalid(String),
}

impl SnapshotExpectation {
    pub fn encode(&self) -> String {
        match self {
            Self::Absent => "absent".into(),
            Self::Matching(id) => format!("id:{id}"),
            Self::Invalid(fingerprint) => format!("invalid:{fingerprint}"),
        }
    }

    pub fn decode(encoded: &str) -> Option<Self> {
        if encoded == "absent" {
            Some(Self::Absent)
        } else if let Some(id) = encoded.strip_prefix("id:").filter(|s| !s.is_empty()) {
            Some(Self::Matching(id.into()))
        } else {
            encoded.strip_prefix("invalid:").filter(|s| !s.is_empty()).map(|f| Self::Invalid(f.into()))
        }
    }

    /// Reads the current state of the saved map for `root`. Call only while holding the
    /// root mutation lease.
    #[cfg(unix)]
    pub fn capture(cache: &Path, root: &[u8]) -> Result<Self> {
        let path = crate::store::saved_map_path(cache, root);
        let Some(info) = sys::lstat(&path)? else { return Ok(Self::Absent) };
        if !info.is_regular() || info.uid != sys::euid() || info.nlink != 1 {
            return Err(unsafe_file("saved map"));
        }
        if let Some(snapshot) = crate::store::read_saved_snapshot(&path, root, true) {
            return Ok(Self::Matching(snapshot.id));
        }
        Ok(Self::Invalid(format!(
            "{}:{}:{}:{}:{}:{}:{}",
            info.dev, info.ino, info.size, info.mtime.0, info.mtime.1, info.ctime.0, info.ctime.1
        )))
    }

    /// Fails with `SnapshotChanged` unless the saved map is still in the expected state.
    #[cfg(unix)]
    pub fn verify(&self, cache: &Path, root: &[u8]) -> Result<()> {
        if Self::capture(cache, root)? == *self { Ok(()) } else { Err(CoreError::SnapshotChanged) }
    }

    /// The saved map was published from the stage sealed as `id` (also after later in-place
    /// updates, which keep the `sealed` row). Call under the mutation lease.
    pub fn descends_from_stage(id: &str, cache: &Path, root: &[u8]) -> Result<bool> {
        let path = crate::store::saved_map_path(cache, root);
        if crate::store::read_saved_snapshot(&path, root, true).is_none() {
            return Ok(false);
        }
        let conn = crate::schema::open_reader(&path, None, true)
            .map_err(|_| CoreError::Invalid("the saved map is unreadable".into()))?;
        use rusqlite::OptionalExtension;
        let exists: Option<i64> = conn
            .query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name='sealed'", [], |row| row.get(0))
            .optional()?;
        if exists.is_none() {
            return Ok(false);
        }
        let sealed: Option<String> =
            conn.query_row("SELECT snapshot_id FROM sealed WHERE singleton=1", [], |row| row.get(0)).optional()?;
        Ok(sealed.as_deref() == Some(id))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn root_id() -> FileIdentity {
        FileIdentity { dev: 7, ino: 42 }
    }

    #[test]
    fn stage_lease_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = StageLease::acquire(dir.path(), b"/r", root_id()).unwrap();
        assert!(matches!(StageLease::acquire(dir.path(), b"/r", root_id()), Err(CoreError::InUse(_))));
        // A different root identity is a different stage.
        let other = StageLease::acquire(dir.path(), b"/r", FileIdentity { dev: 7, ino: 43 }).unwrap();
        assert_ne!(first.database_path(), other.database_path());
        drop(first);
        StageLease::acquire(dir.path(), b"/r", root_id()).unwrap();
    }

    #[test]
    fn stage_lease_detects_replaced_or_unsafe_files() {
        let dir = tempfile::tempdir().unwrap();
        let lease = StageLease::acquire(dir.path(), b"/r", root_id()).unwrap();
        let db = lease.database_path().to_path_buf();
        // Replace the database with a different file of the same name.
        std::fs::remove_file(&db).unwrap();
        std::fs::write(&db, b"").unwrap();
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(lease.validate_paths().is_err());
        assert!(lease.remove_stage_files().is_err());
        drop(lease);

        // A symlinked database is refused at acquisition.
        std::fs::remove_file(&db).unwrap();
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), &db).unwrap();
        assert!(StageLease::acquire(dir.path(), b"/r", root_id()).is_err());
        std::fs::remove_file(&db).unwrap();

        // A group-readable sidecar is refused.
        let lease = StageLease::acquire(dir.path(), b"/r", root_id()).unwrap();
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        std::fs::write(&wal, b"").unwrap();
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(lease.validate_paths().is_err());
        std::fs::set_permissions(&wal, std::fs::Permissions::from_mode(0o600)).unwrap();
        lease.validate_paths().unwrap();
        lease.remove_stage_files().unwrap();
        assert!(!db.exists() && !wal.exists());
    }

    #[test]
    fn stage_lease_rejects_incompatible_stage_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = stage_database_path(dir.path(), b"/r", root_id());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::set_permissions(path.parent().unwrap(), std::fs::Permissions::from_mode(0o700)).unwrap();
        rusqlite::Connection::open(&path).unwrap().execute_batch("PRAGMA user_version=5; CREATE TABLE t(x)").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(StageLease::acquire(dir.path(), b"/r", root_id()).is_err());
    }

    #[test]
    fn root_mutation_lease_is_exclusive_and_invalidation_marker_is_durable() {
        let dir = tempfile::tempdir().unwrap();
        let lease = RootMutationLease::try_acquire(dir.path(), b"/r").unwrap().unwrap();
        assert!(RootMutationLease::try_acquire(dir.path(), b"/r").unwrap().is_none());
        // A cancelled waiter returns promptly instead of blocking behind the holder.
        let cancel = Cancellation::new();
        cancel.cancel();
        assert!(matches!(RootMutationLease::acquire(dir.path(), b"/r", &cancel), Err(CoreError::Cancelled)));
        assert_eq!(lease.invalidation_generation().unwrap(), "0");
        lease.invalidate_generation().unwrap();
        let first = lease.invalidation_generation().unwrap();
        assert_ne!(first, "0");
        drop(lease);
        let again = RootMutationLease::try_acquire(dir.path(), b"/r").unwrap().unwrap();
        assert_eq!(again.invalidation_generation().unwrap(), first);
        // A replaced lock file is detected.
        let lock = dir.path().join(LOCKS_DIR).join(format!("{}.lock", crate::schema::root_digest(b"/r")));
        std::fs::remove_file(&lock).unwrap();
        assert!(again.validate().is_err());
    }

    #[test]
    fn expectation_round_trips_and_rejects_garbage() {
        for value in [
            SnapshotExpectation::Absent,
            SnapshotExpectation::Matching("A".into()),
            SnapshotExpectation::Invalid("1:2".into()),
        ] {
            assert_eq!(SnapshotExpectation::decode(&value.encode()), Some(value));
        }
        assert_eq!(SnapshotExpectation::decode("id:"), None);
        assert_eq!(SnapshotExpectation::decode("unspecified"), None);
    }
}
