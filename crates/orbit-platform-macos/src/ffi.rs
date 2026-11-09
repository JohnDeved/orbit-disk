//! Raw declarations for the Apple APIs the `libc` crate does not cover: CoreFoundation strings and
//! arrays, CoreServices FSEvents, IOKit registry lookups, libdispatch queues and I/O policy.
#![allow(non_camel_case_types, non_upper_case_globals, non_snake_case)]

use std::ffi::{c_char, c_int, c_void};

// --- sys/attr.h, sys/stat.h, sys/resource.h ------------------------------------------------

pub const ATTR_CMN_ERROR: u32 = 0x2000_0000;
pub const SF_DATALESS: u32 = 0x4000_0000;
pub const VDIR: u32 = 2;
pub const VLNK: u32 = 5;
pub const VREG: u32 = 1;
pub const CLONE_NOFOLLOW: u32 = 0x0001;

pub const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
pub const IOPOL_SCOPE_THREAD: c_int = 1;
pub const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;

unsafe extern "C" {
    pub fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
    pub fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
}

// --- CoreFoundation -------------------------------------------------------------------------

pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFArrayRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CFUUIDRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFIndex = isize;
pub type Boolean = u8;

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CFUUIDBytes {
    pub bytes: [u8; 16],
}

#[repr(C)]
pub struct CFArrayCallBacks {
    _private: [u8; 0],
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFTypeArrayCallBacks: CFArrayCallBacks;
    pub fn CFRelease(value: CFTypeRef);
    pub fn CFStringCreateWithBytes(
        allocator: CFAllocatorRef,
        bytes: *const u8,
        length: CFIndex,
        encoding: u32,
        external: Boolean,
    ) -> CFStringRef;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFGetTypeID(value: CFTypeRef) -> usize;
    pub fn CFStringGetLength(value: CFStringRef) -> CFIndex;
    pub fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    pub fn CFStringGetCString(value: CFStringRef, buffer: *mut c_char, size: CFIndex, encoding: u32) -> Boolean;
    pub fn CFArrayCreate(
        allocator: CFAllocatorRef,
        values: *const CFTypeRef,
        count: CFIndex,
        callbacks: *const CFArrayCallBacks,
    ) -> CFArrayRef;
    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
}

// --- CoreServices FSEvents ------------------------------------------------------------------

pub type FSEventStreamRef = *mut c_void;
pub type ConstFSEventStreamRef = *const c_void;
pub type FSEventStreamCallback = extern "C" fn(
    stream: ConstFSEventStreamRef,
    info: *mut c_void,
    count: usize,
    paths: *mut c_void,
    flags: *const u32,
    ids: *const u64,
);

#[repr(C)]
pub struct FSEventStreamContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: Option<extern "C" fn(*const c_void) -> *const c_void>,
    pub release: Option<extern "C" fn(*const c_void)>,
    pub copy_description: Option<extern "C" fn(*const c_void) -> CFStringRef>,
}

pub const kFSEventStreamEventIdSinceNow: u64 = u64::MAX;

pub const kFSEventStreamCreateFlagNoDefer: u32 = 0x02;
pub const kFSEventStreamCreateFlagWatchRoot: u32 = 0x04;
pub const kFSEventStreamCreateFlagFileEvents: u32 = 0x10;

pub const kFSEventStreamEventFlagMustScanSubDirs: u32 = 0x0000_0001;
pub const kFSEventStreamEventFlagUserDropped: u32 = 0x0000_0002;
pub const kFSEventStreamEventFlagKernelDropped: u32 = 0x0000_0004;
pub const kFSEventStreamEventFlagEventIdsWrapped: u32 = 0x0000_0008;
pub const kFSEventStreamEventFlagHistoryDone: u32 = 0x0000_0010;
pub const kFSEventStreamEventFlagRootChanged: u32 = 0x0000_0020;
pub const kFSEventStreamEventFlagMount: u32 = 0x0000_0040;
pub const kFSEventStreamEventFlagUnmount: u32 = 0x0000_0080;
#[cfg_attr(not(test), allow(dead_code))]
pub const kFSEventStreamEventFlagItemCreated: u32 = 0x0000_0100;
pub const kFSEventStreamEventFlagItemRemoved: u32 = 0x0000_0200;
pub const kFSEventStreamEventFlagItemRenamed: u32 = 0x0000_0800;
pub const kFSEventStreamEventFlagItemIsDir: u32 = 0x0002_0000;

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    pub fn FSEventStreamCreate(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *const FSEventStreamContext,
        paths: CFArrayRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    pub fn FSEventStreamCreateRelativeToDevice(
        allocator: CFAllocatorRef,
        callback: FSEventStreamCallback,
        context: *const FSEventStreamContext,
        device: libc::dev_t,
        paths: CFArrayRef,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> FSEventStreamRef;
    pub fn FSEventStreamSetDispatchQueue(stream: FSEventStreamRef, queue: dispatch_queue_t);
    pub fn FSEventStreamStart(stream: FSEventStreamRef) -> Boolean;
    pub fn FSEventStreamFlushSync(stream: FSEventStreamRef);
    pub fn FSEventStreamStop(stream: FSEventStreamRef);
    pub fn FSEventStreamInvalidate(stream: FSEventStreamRef);
    pub fn FSEventStreamRelease(stream: FSEventStreamRef);
    pub fn FSEventsGetCurrentEventId() -> u64;
    pub fn FSEventsGetLastEventIdForDeviceBeforeTime(device: libc::dev_t, time: f64) -> u64;
    pub fn FSEventsCopyUUIDForDevice(device: libc::dev_t) -> CFUUIDRef;
}

// --- libdispatch (part of libSystem) --------------------------------------------------------

pub type dispatch_queue_t = *mut c_void;

unsafe extern "C" {
    pub fn dispatch_queue_create(label: *const c_char, attr: *const c_void) -> dispatch_queue_t;
    pub fn dispatch_async_f(queue: dispatch_queue_t, context: *mut c_void, work: extern "C" fn(*mut c_void));
    pub fn dispatch_release(object: *mut c_void);
}

// --- IOKit ----------------------------------------------------------------------------------

pub type io_object_t = u32;
pub const kIOMainPortDefault: u32 = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    pub fn IOServiceMatching(name: *const c_char) -> CFMutableDictionaryRef;
    pub fn IOServiceGetMatchingService(main_port: u32, matching: CFMutableDictionaryRef) -> io_object_t;
    pub fn IORegistryEntryCreateCFProperty(
        entry: io_object_t,
        key: CFStringRef,
        allocator: CFAllocatorRef,
        options: u32,
    ) -> CFTypeRef;
    pub fn IOObjectRelease(object: io_object_t) -> c_int;
}

// --- Safe helpers ---------------------------------------------------------------------------

/// An owned CoreFoundation reference released on drop.
pub struct CfOwned(pub CFTypeRef);

impl Drop for CfOwned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: we own one retain count on a non-null CF object.
            unsafe { CFRelease(self.0) }
        }
    }
}

/// Creates a CFString from raw bytes, interpreted as UTF-8. `None` for invalid UTF-8.
pub fn cf_string(bytes: &[u8]) -> Option<CfOwned> {
    // SAFETY: `bytes` is a valid slice for the given length; CF copies it.
    let value = unsafe {
        CFStringCreateWithBytes(std::ptr::null(), bytes.as_ptr(), bytes.len() as CFIndex, kCFStringEncodingUTF8, 0)
    };
    (!value.is_null()).then(|| CfOwned(value))
}

/// Copies a CFString into a Rust string. `None` when `value` is not a CFString.
pub fn string_from_cf(value: CFTypeRef) -> Option<String> {
    if value.is_null() {
        return None;
    }
    // SAFETY: `value` is a live CF object; type IDs are plain integers.
    if unsafe { CFGetTypeID(value) != CFStringGetTypeID() } {
        return None;
    }
    // SAFETY: `value` is a CFString (checked above).
    let length = unsafe { CFStringGetLength(value) };
    // SAFETY: pure computation.
    let capacity = unsafe { CFStringGetMaximumSizeForEncoding(length, kCFStringEncodingUTF8) } + 1;
    let mut buffer = vec![0u8; capacity.max(1) as usize];
    // SAFETY: `buffer` is writable for `capacity` bytes.
    let ok = unsafe { CFStringGetCString(value, buffer.as_mut_ptr().cast(), capacity, kCFStringEncodingUTF8) };
    if ok == 0 {
        return None;
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    buffer.truncate(end);
    String::from_utf8(buffer).ok()
}

/// A CFArray of CFStrings (one per path). `None` if a path is not valid UTF-8.
pub fn cf_string_array(items: &[&[u8]]) -> Option<CfOwned> {
    let strings: Vec<CfOwned> = items.iter().map(|item| cf_string(item)).collect::<Option<_>>()?;
    let raw: Vec<CFTypeRef> = strings.iter().map(|s| s.0).collect();
    // SAFETY: `raw` holds live CF objects; the type callbacks retain them for the array.
    let array = unsafe {
        CFArrayCreate(std::ptr::null(), raw.as_ptr(), raw.len() as CFIndex, &raw const kCFTypeArrayCallBacks)
    };
    (!array.is_null()).then(|| CfOwned(array))
}

/// Formats a UUID as uppercase hyphenated hex, matching `CFUUIDCreateString`.
pub fn format_uuid(bytes: &[u8; 16]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(36);
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        let _ = write!(out, "{byte:02X}");
    }
    out
}
