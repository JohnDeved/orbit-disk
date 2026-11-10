//! Cloud storage, file operations, volumes, locations and machine identity.

use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use objc2::Message;
use objc2::rc::{Retained, autoreleasepool};
use objc2_foundation::{
    NSError, NSFileManager, NSNumber, NSString, NSURL, NSURLIsUbiquitousItemKey, NSURLResourceKey,
    NSURLVolumeIsEjectableKey, NSURLVolumeIsRemovableKey, NSURLVolumeNameKey,
};
use orbit_platform::{AppDirs, CloudStatus, PlatformError, ProtectedPaths, Result, VolumeInfo};

use crate::ffi;
use crate::paths;

const BUNDLE_ID: &str = "local.orbitdisk.app";

// --- Materialization policy -----------------------------------------------------------------

/// Disables on-demand download of dataless files for the current thread while `body` runs and
/// restores the previous thread policy afterwards (also on unwind).
pub fn without_materialization<R>(body: impl FnOnce() -> R) -> Result<R> {
    // SAFETY: plain syscall wrapper.
    let previous =
        unsafe { ffi::getiopolicy_np(ffi::IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, ffi::IOPOL_SCOPE_THREAD) };
    if previous < 0 {
        return Err(PlatformError::last_os(""));
    }
    struct Restore(libc::c_int);
    impl Drop for Restore {
        fn drop(&mut self) {
            // SAFETY: plain syscall wrapper restoring the saved thread policy.
            unsafe {
                ffi::setiopolicy_np(ffi::IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, ffi::IOPOL_SCOPE_THREAD, self.0)
            };
        }
    }
    let _restore = if previous != ffi::IOPOL_MATERIALIZE_DATALESS_FILES_OFF {
        // SAFETY: plain syscall wrapper.
        let status = unsafe {
            ffi::setiopolicy_np(
                ffi::IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                ffi::IOPOL_SCOPE_THREAD,
                ffi::IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            )
        };
        if status != 0 {
            return Err(PlatformError::last_os(""));
        }
        Some(Restore(previous))
    } else {
        None
    };
    Ok(body())
}

/// The current thread's dataless-materialization policy (tests).
pub fn materialization_policy() -> libc::c_int {
    // SAFETY: plain syscall wrapper.
    unsafe { ffi::getiopolicy_np(ffi::IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, ffi::IOPOL_SCOPE_THREAD) }
}

// --- Cloud storage --------------------------------------------------------------------------

fn file_url(path: &Path) -> Option<Retained<NSURL>> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let is_dir = paths::lstat(path.as_os_str().as_bytes()).is_some_and(|info| paths::is_dir(&info));
    // SAFETY: `c` is a valid NUL-terminated path for the duration of the call.
    Some(unsafe {
        NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
            NonNull::new_unchecked(c.as_ptr().cast_mut()),
            is_dir,
            None,
        )
    })
}

fn path_from_url(url: &NSURL) -> PathBuf {
    let raw = url.fileSystemRepresentation();
    // SAFETY: NSURL returns a NUL-terminated C string that lives as long as the autorelease pool.
    let bytes = unsafe { CStr::from_ptr(raw.as_ptr()) }.to_bytes();
    paths::to_path(bytes)
}

fn resource_bool(url: &NSURL, key: &NSURLResourceKey) -> std::result::Result<Option<bool>, Retained<NSError>> {
    let mut value = None;
    // SAFETY: the out-parameter receives an NSNumber or nil for boolean keys.
    unsafe { url.getResourceValue_forKey_error(&mut value, key) }?;
    Ok(value.and_then(|value| value.downcast::<NSNumber>().ok()).map(|number| number.boolValue()))
}

fn resource_string(url: &NSURL, key: &NSURLResourceKey) -> Option<String> {
    let mut value = None;
    // SAFETY: the out-parameter receives an NSString or nil for string keys.
    unsafe { url.getResourceValue_forKey_error(&mut value, key) }.ok()?;
    value.and_then(|value| value.downcast::<NSString>().ok()).map(|string| string.to_string())
}

/// Error code of an NSError, preferring an underlying POSIX error.
fn ns_error_code(error: &NSError) -> i32 {
    let mut current: Retained<NSError> = error.retain();
    for _ in 0..8 {
        if current.domain().to_string() == "NSPOSIXErrorDomain" {
            return current.code() as i32;
        }
        let info = current.userInfo();
        let underlying = info
            .objectForKey(&NSString::from_str("NSUnderlyingError"))
            .and_then(|value| value.downcast::<NSError>().ok());
        match underlying {
            Some(next) => current = next,
            None => break,
        }
    }
    // NSCocoaErrorDomain file errors.
    match error.code() {
        4 | 260 => libc::ENOENT,
        257 | 513 => libc::EACCES,
        516 => libc::EEXIST,
        640 => libc::ENOSPC,
        _ => libc::EIO,
    }
}

pub fn cloud_status(path: &Path) -> Result<CloudStatus> {
    without_materialization(|| {
        let raw = path.as_os_str().as_bytes();
        let c = CString::new(raw).map_err(|_| PlatformError::os(path, libc::EINVAL))?;
        // SAFETY: zeroed stat is valid POD.
        let mut info: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: NUL-terminated path, writable out-param.
        if orbit_platform::posix::retry_interrupted(|| unsafe { libc::lstat(c.as_ptr(), &mut info) }) != 0 {
            let error = PlatformError::last_os(path);
            return if error.code() == Some(libc::EDEADLK) { Ok(CloudStatus::Dataless) } else { Err(error) };
        }
        if info.st_flags & ffi::SF_DATALESS != 0 {
            return Ok(CloudStatus::Dataless);
        }
        autoreleasepool(|_| {
            let url = file_url(path).ok_or_else(|| PlatformError::os(path, libc::EINVAL))?;
            // SAFETY: reading an immutable Foundation constant.
            let key = unsafe { NSURLIsUbiquitousItemKey };
            match resource_bool(&url, key) {
                Ok(Some(true)) => Ok(CloudStatus::Cloud),
                Ok(_) => Ok(CloudStatus::Local),
                Err(error) => match ns_error_code(&error) {
                    libc::EDEADLK => Ok(CloudStatus::Dataless),
                    code => Err(PlatformError::os(path, code)),
                },
            }
        })
    })?
}

/// Known provider roots plus Desktop/Documents when iCloud syncs them, with resolved aliases.
pub fn cloud_roots(home: &Path) -> Vec<PathBuf> {
    let home = paths::standardize(home.as_os_str().as_bytes());
    let mut roots: Vec<Vec<u8>> = ["Library/CloudStorage", "Library/Mobile Documents"]
        .iter()
        .map(|relative| paths::child(&home, relative.as_bytes()))
        .collect();
    for name in ["Desktop", "Documents"] {
        let candidate = paths::child(&home, name.as_bytes());
        if matches!(cloud_status(&paths::to_path(&candidate)), Ok(CloudStatus::Cloud | CloudStatus::Dataless)) {
            roots.push(candidate);
        }
    }
    let mut out: Vec<Vec<u8>> = Vec::new();
    for root in roots {
        let resolved = paths::realpath(&root).ok();
        for path in std::iter::once(root).chain(resolved) {
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out.iter().map(|p| paths::to_path(p)).collect()
}

// --- File operations ------------------------------------------------------------------------

fn c_path(path: &Path) -> Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| PlatformError::os(path, libc::EINVAL))
}

/// `clonefile(2)`; on failure (other volume, unsupported filesystem) a full copy into a newly
/// created `dst`. Never replaces an existing `dst`.
pub fn clone_file(src: &Path, dst: &Path) -> Result<()> {
    let (c_src, c_dst) = (c_path(src)?, c_path(dst)?);
    // SAFETY: NUL-terminated paths.
    if unsafe { libc::clonefile(c_src.as_ptr(), c_dst.as_ptr(), ffi::CLONE_NOFOLLOW) } == 0 {
        return Ok(());
    }
    let clone_error = PlatformError::last_os(dst);
    if clone_error.code() == Some(libc::EEXIST) || paths::lstat(dst.as_os_str().as_bytes()).is_some() {
        return Err(PlatformError::os(dst, libc::EEXIST));
    }
    copy_exclusive(src, dst)
}

pub(crate) fn copy_exclusive(src: &Path, dst: &Path) -> Result<()> {
    let mut input =
        std::fs::File::open(src).map_err(|e| PlatformError::os(src, e.raw_os_error().unwrap_or(libc::EIO)))?;
    let mode = input.metadata().map(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions())).unwrap_or(0o600);
    let c_dst = c_path(dst)?;
    // SAFETY: NUL-terminated path; O_EXCL refuses to follow or replace anything at `dst`.
    let fd = unsafe {
        libc::open(
            c_dst.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600 as libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(PlatformError::last_os(dst));
    }
    // SAFETY: `open` returned a fresh descriptor we own.
    let mut output = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
    let result = std::io::copy(&mut input, &mut output).and_then(|_| output.sync_all()).and_then(|_| {
        // SAFETY: live descriptor.
        if unsafe { libc::fchmod(output.as_raw_fd(), (mode & 0o7777) as libc::mode_t) } != 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    });
    if let Err(error) = result {
        drop(output);
        let _ = std::fs::remove_file(dst);
        return Err(PlatformError::os(dst, error.raw_os_error().unwrap_or(libc::EIO)));
    }
    Ok(())
}

pub fn trash(path: &Path) -> Result<Option<PathBuf>> {
    autoreleasepool(|_| {
        let url = file_url(path).ok_or_else(|| PlatformError::os(path, libc::EINVAL))?;
        let manager = NSFileManager::defaultManager();
        let mut resulting: Option<Retained<NSURL>> = None;
        manager
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut resulting))
            .map_err(|error| PlatformError::os(path, ns_error_code(&error)))?;
        Ok(resulting.map(|url| path_from_url(&url)))
    })
}

// --- Volumes --------------------------------------------------------------------------------

pub fn volumes() -> Vec<VolumeInfo> {
    let mut out: Vec<VolumeInfo> = Vec::new();
    let mut mounts = paths::mounted_filesystems();
    // The boot volume first, then /Volumes/* in mount-name order.
    mounts.sort_by_key(|info| {
        let name = paths::mount_on_name(info);
        (name != b"/", name)
    });
    for info in mounts {
        let mount = paths::mount_on_name(&info);
        let flags = info.f_flags as libc::c_int;
        let is_root = mount == b"/";
        if !is_root && (!mount.starts_with(b"/Volumes/") || flags & libc::MNT_DONTBROWSE != 0) {
            continue;
        }
        if out.iter().any(|volume| paths::bytes(&volume.path) == mount) {
            continue;
        }
        let block = u64::from(info.f_bsize);
        let path = paths::to_path(&mount);
        let (name, removable) = autoreleasepool(|_| {
            let Some(url) = file_url(&path) else { return (None, false) };
            // SAFETY: reading immutable Foundation constants.
            let (name_key, removable_key, ejectable_key) =
                unsafe { (NSURLVolumeNameKey, NSURLVolumeIsRemovableKey, NSURLVolumeIsEjectableKey) };
            let removable = resource_bool(&url, removable_key).ok().flatten().unwrap_or(false)
                || resource_bool(&url, ejectable_key).ok().flatten().unwrap_or(false);
            (resource_string(&url, name_key), removable)
        });
        let fallback_name = || {
            if is_root { "Macintosh HD".to_string() } else { paths::lossy(&mount[b"/Volumes/".len()..]) }
        };
        out.push(VolumeInfo {
            path,
            name: name.unwrap_or_else(fallback_name),
            total_bytes: info.f_blocks.saturating_mul(block),
            available_bytes: info.f_bavail.saturating_mul(block),
            is_boot: is_root,
            is_removable: removable,
            is_local: flags & libc::MNT_LOCAL != 0,
        });
    }
    out
}

// --- Identity & locations -------------------------------------------------------------------

/// `IOPlatformUUID` of the platform expert device.
pub fn machine_identity() -> Option<String> {
    // SAFETY: IOKit calls with valid arguments; the matching dictionary is consumed by
    // IOServiceGetMatchingService, the service and property are released below.
    unsafe {
        let matching = ffi::IOServiceMatching(c"IOPlatformExpertDevice".as_ptr());
        if matching.is_null() {
            return None;
        }
        let service = ffi::IOServiceGetMatchingService(ffi::kIOMainPortDefault, matching);
        if service == 0 {
            return None;
        }
        let key = ffi::cf_string(b"IOPlatformUUID");
        let value =
            key.map(|key| ffi::CfOwned(ffi::IORegistryEntryCreateCFProperty(service, key.0, std::ptr::null(), 0)));
        ffi::IOObjectRelease(service);
        value.and_then(|value| ffi::string_from_cf(value.0))
    }
}

pub fn home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").filter(|h| Path::new(h).is_absolute()) {
        return PathBuf::from(home);
    }
    // SAFETY: getpwuid returns a pointer into static storage or null; we copy immediately.
    unsafe {
        let entry = libc::getpwuid(libc::geteuid());
        if !entry.is_null() && !(*entry).pw_dir.is_null() {
            return paths::to_path(CStr::from_ptr((*entry).pw_dir).to_bytes());
        }
    }
    PathBuf::from("/")
}

/// `confstr(_CS_DARWIN_USER_TEMP_DIR)`, or `/tmp` when unavailable.
pub fn user_temp_dir() -> PathBuf {
    // SAFETY: a null buffer asks for the required length.
    let length = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, std::ptr::null_mut(), 0) };
    if length > 0 {
        let mut buffer = vec![0u8; length];
        // SAFETY: `buffer` holds `length` writable bytes.
        let written = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buffer.as_mut_ptr().cast(), length) };
        if written > 0 {
            let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
            return paths::to_path(&buffer[..end]);
        }
    }
    PathBuf::from("/tmp")
}

pub fn app_dirs() -> AppDirs {
    let home = home_dir();
    let support = home.join("Library/Application Support").join(BUNDLE_ID);
    let caches = home.join("Library/Caches").join(BUNDLE_ID);
    AppDirs {
        cache: caches.join("Maps"),
        support: support.join("Engine"),
        // Swift `IndexLocations.locks(for:)`: the removal journal lives in `<support>/Mapping`.
        removal_journal: support.join("Mapping"),
        // Swift format-5 saved maps; the user chose to discard them rather than migrate.
        obsolete_caches: vec![caches.join("Indexes")],
        settings: support.join("Settings"),
        // Must match the Swift automation endpoint so existing clients find the socket.
        // SAFETY: geteuid has no preconditions.
        runtime: user_temp_dir().join(format!("orbit-disk-automation-{}", unsafe { libc::geteuid() })),
    }
}

fn with_resolved(paths_in: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in paths_in {
        let resolved = paths::realpath(path.as_os_str().as_bytes()).ok().map(|p| paths::to_path(&p));
        for candidate in std::iter::once(path).chain(resolved) {
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

pub fn protected_paths(home: &Path) -> ProtectedPaths {
    let system = ["/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc"].map(PathBuf::from);
    let mut system_roots = with_resolved(system);
    for path in with_resolved([home.join("Library")]) {
        if !system_roots.contains(&path) {
            system_roots.push(path);
        }
    }
    let temp = paths::to_path(&paths::standardize(user_temp_dir().as_os_str().as_bytes()));
    let mut temporary_roots = with_resolved([temp, PathBuf::from("/private/tmp")]);
    // Foundation strips "/private" when resolving; keep that spelling too.
    let stripped: Vec<PathBuf> = temporary_roots
        .iter()
        .filter_map(|p| p.strip_prefix("/private").ok().map(|rest| Path::new("/").join(rest)))
        .collect();
    for path in stripped {
        if !temporary_roots.contains(&path) {
            temporary_roots.push(path);
        }
    }
    ProtectedPaths {
        system_roots,
        temporary_roots,
        restricted_root: Some(PathBuf::from("/private")),
        trash_components: vec![".Trash", ".Trashes"],
        volumes_root: Some(PathBuf::from("/Volumes")),
    }
}
