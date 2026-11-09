//! Color management for GPUI windows.

use std::ffi::c_void;

use objc2::encode::{Encoding, RefEncode};
use objc2::msg_send;
use objc2::runtime::AnyObject;

/// Opaque `CGColorSpace`, encoded as `^{CGColorSpace=}` so objc2's message type checks pass.
#[repr(C)]
pub(crate) struct CGColorSpace {
    _private: [u8; 0],
}

// SAFETY: matches the Objective-C type encoding of `CGColorSpaceRef`.
unsafe impl RefEncode for CGColorSpace {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Encoding::Struct("CGColorSpace", &[]));
}

type CGColorSpaceRef = *mut CGColorSpace;
type CFStringRef = *const c_void;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: CFStringRef;
    fn CGColorSpaceCreateWithName(name: CFStringRef) -> CGColorSpaceRef;
    fn CGColorSpaceRelease(space: CGColorSpaceRef);
}

/// Tags the backing `CAMetalLayer` of `ns_view` as sRGB. GPUI writes sRGB-encoded colors into
/// a `BGRA8Unorm` drawable but leaves `layer.colorspace` nil, so macOS shows the values with
/// the display's own primaries (desaturated/shifted on many monitors). Tagging the layer makes
/// the compositor color-match it like any AppKit content.
///
/// # Safety
/// `ns_view` must be a live `NSView` pointer; call on the main thread.
pub(crate) unsafe fn tag_layer_srgb(ns_view: *mut c_void) {
    let view = ns_view.cast::<AnyObject>();
    if view.is_null() {
        return;
    }
    // SAFETY: `view` is a live NSView (caller contract); `-layer` returns the backing layer or nil.
    let layer: *mut AnyObject = unsafe { msg_send![&*view, layer] };
    if layer.is_null() {
        return;
    }
    // SAFETY: CoreGraphics returns a +1 color space for the named constant; released below
    // after the layer has retained it via the `colorspace` property.
    unsafe {
        let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
        if space.is_null() {
            return;
        }
        let () = msg_send![&*layer, setColorspace: space];
        CGColorSpaceRelease(space);
    }
}
