//! Sparkle 2 updater, loaded at runtime from `<bundle>/Contents/Frameworks/Sparkle.framework`.
//!
//! Port of `Sources/OrbitDisk/AppUpdater.swift`. Sparkle is not linked: the framework is loaded
//! with `NSBundle` and its classes are looked up by name, so unbundled development builds run
//! without it (and without an updater). The delegate refuses update checks and installs while a
//! removal is running and postpones Sparkle's relaunch until the removal finishes.

use std::cell::{Cell, RefCell};

use block2::{DynBlock, RcBlock};
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_foundation::{
    NSBundle, NSDictionary, NSError, NSInteger, NSLocalizedDescriptionKey, NSLocalizedRecoverySuggestionErrorKey,
    NSObject, NSObjectNSThreadPerformAdditions, NSString,
};
use orbit_platform::{Updater, UpdaterState};

#[derive(Default)]
struct DelegateState {
    /// A removal is in progress (`DiskModel.hasActiveRemovals` in the Swift app).
    blocked: Cell<bool>,
    /// Sparkle's install-and-relaunch handler, held while a removal runs.
    pending_relaunch: RefCell<Option<RcBlock<dyn Fn()>>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, the class adds no `Drop` logic that
    // touches Objective-C state, and every method below matches the selector's C signature in
    // Sparkle 2's `SPUUpdaterDelegate.h` (BOOL return, NSInteger `SPUUpdateCheck`).
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "OrbitDiskUpdaterDelegate"]
    #[ivars = DelegateState]
    struct UpdaterDelegate;

    // SAFETY: NSObjectProtocol has no additional requirements.
    unsafe impl NSObjectProtocol for UpdaterDelegate {}

    // Sparkle checks `respondsToSelector:` for each optional SPUUpdaterDelegate method, so formal
    // protocol conformance is not needed.
    impl UpdaterDelegate {
        #[unsafe(method(updater:mayPerformUpdateCheck:error:))]
        fn may_perform_update_check(&self, _updater: &AnyObject, _check: NSInteger, error: *mut *mut NSError) -> bool {
            self.require_removal_finished(error)
        }

        #[unsafe(method(updater:shouldProceedWithUpdate:updateCheck:error:))]
        fn should_proceed_with_update(
            &self,
            _updater: &AnyObject,
            _item: &AnyObject,
            _check: NSInteger,
            error: *mut *mut NSError,
        ) -> bool {
            self.require_removal_finished(error)
        }

        #[unsafe(method(updater:shouldPostponeRelaunchForUpdate:untilInvokingBlock:))]
        fn should_postpone_relaunch(
            &self,
            _updater: &AnyObject,
            _item: &AnyObject,
            install_handler: &DynBlock<dyn Fn()>,
        ) -> bool {
            // (No early `return`: the macro rewrites the `bool` return type to `Bool`.)
            let blocked = self.ivars().blocked.get();
            if blocked {
                *self.ivars().pending_relaunch.borrow_mut() = Some(install_handler.copy());
            }
            blocked
        }

        #[unsafe(method(updater:didAbortWithError:))]
        fn did_abort(&self, _updater: &AnyObject, _error: &NSError) {
            self.ivars().pending_relaunch.borrow_mut().take();
        }

        /// Runs on a later main-loop turn after unblocking, never inside the caller's stack.
        #[unsafe(method(orbitResumePendingRelaunch))]
        fn resume_pending_relaunch(&self) {
            if self.ivars().blocked.get() {
                return;
            }
            let handler = self.ivars().pending_relaunch.borrow_mut().take();
            if let Some(handler) = handler {
                handler.call(());
            }
        }
    }
);

impl UpdaterDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateState::default());
        // SAFETY: `-[NSObject init]` is the designated initializer of the superclass.
        unsafe { msg_send![super(this), init] }
    }

    /// Swift's throwing `requireRemovalFinished()`: `NO` plus an `NSError` while blocked.
    fn require_removal_finished(&self, error: *mut *mut NSError) -> bool {
        if !self.ivars().blocked.get() {
            return true;
        }
        if !error.is_null() {
            // SAFETY: the keys are immutable `NSString` constants exported by Foundation.
            let keys = unsafe { [NSLocalizedDescriptionKey, NSLocalizedRecoverySuggestionErrorKey] };
            let description = NSString::from_str("Files are still being removed.");
            let suggestion = NSString::from_str("Check for updates after file removal finishes.");
            let info = NSDictionary::<NSString, AnyObject>::from_slices(&keys, &[&*description, &*suggestion]);
            // SAFETY: the user-info dictionary holds only `NSString` values for string keys.
            let value = unsafe {
                NSError::errorWithDomain_code_userInfo(&NSString::from_str("OrbitDisk.Updates"), 1, Some(&info))
            };
            // SAFETY: Sparkle passes a valid `NSError * __autoreleasing *`; an autoreleased
            // object is what such an out-parameter expects.
            unsafe { *error = Retained::autorelease_ptr(value) };
        }
        false
    }
}

/// `SPUStandardUpdaterController` plus the blocking delegate. Main thread only (not `Send`).
pub struct SparkleUpdater {
    controller: Retained<AnyObject>,
    updater: Retained<AnyObject>,
    delegate: Retained<UpdaterDelegate>,
    started: Cell<bool>,
}

impl std::fmt::Debug for SparkleUpdater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SparkleUpdater").field("started", &self.started.get()).finish_non_exhaustive()
    }
}

impl SparkleUpdater {
    /// Loads Sparkle from the main bundle's private frameworks directory and creates an
    /// unstarted controller. `None` when the framework is absent or fails to load.
    pub(crate) fn load(mtm: MainThreadMarker) -> Option<Self> {
        let frameworks = NSBundle::mainBundle().privateFrameworksPath()?;
        let path = NSString::from_str(&format!("{frameworks}/Sparkle.framework"));
        let bundle = NSBundle::bundleWithPath(&path)?;
        // SAFETY: loading runs the framework's initializers; Sparkle is designed to be loaded
        // into its host app on the main thread, which `mtm` proves we are on.
        if let Err(error) = unsafe { bundle.loadAndReturnError() } {
            eprintln!("orbit-shell-macos: could not load {path}: {}", error.localizedDescription());
            return None;
        }
        let class = AnyClass::get(c"SPUStandardUpdaterController")?;
        let delegate = UpdaterDelegate::new(mtm);
        // SAFETY: `alloc` on a class object returns an owned, uninitialized instance.
        let allocated: Allocated<AnyObject> = unsafe { msg_send![class, alloc] };
        // SAFETY: signature of `-initWithStartingUpdater:updaterDelegate:userDriverDelegate:`
        // (BOOL, id, id). The controller keeps the delegate weakly; `self` keeps it alive.
        let controller: Option<Retained<AnyObject>> = unsafe {
            msg_send![
                allocated,
                initWithStartingUpdater: false,
                updaterDelegate: &*delegate,
                userDriverDelegate: None::<&AnyObject>,
            ]
        };
        let controller = controller?;
        // SAFETY: `-[SPUStandardUpdaterController updater]` returns a non-null `SPUUpdater *`.
        let updater: Retained<AnyObject> = unsafe { msg_send![&*controller, updater] };
        Some(Self { controller, updater, delegate, started: Cell::new(false) })
    }

    /// Starts Sparkle's scheduler once. The Swift app does this when its window appears.
    pub fn start(&self) {
        if !self.started.replace(true) {
            // SAFETY: `-startUpdater` takes no arguments and returns void.
            unsafe {
                let () = msg_send![&*self.controller, startUpdater];
            }
        }
    }

    /// A downloaded update is waiting for the running removal before it relaunches the app.
    pub fn is_waiting_for_removal(&self) -> bool {
        self.delegate.ivars().pending_relaunch.borrow().is_some()
    }
}

impl Updater for SparkleUpdater {
    fn state(&self) -> UpdaterState {
        // SAFETY: both are BOOL properties of `SPUUpdater`.
        let (can_check, automatic_checks): (bool, bool) = unsafe {
            (msg_send![&*self.updater, canCheckForUpdates], msg_send![&*self.updater, automaticallyChecksForUpdates])
        };
        UpdaterState { automatic_checks, can_check: can_check && !self.delegate.ivars().blocked.get() }
    }

    fn set_automatic_checks(&self, enabled: bool) {
        // SAFETY: BOOL property setter of `SPUUpdater`.
        unsafe {
            let () = msg_send![&*self.updater, setAutomaticallyChecksForUpdates: enabled];
        }
    }

    fn check_now(&self) {
        if !self.state().can_check {
            return;
        }
        // SAFETY: `-checkForUpdates:` is an IBAction taking a nullable sender.
        unsafe {
            let () = msg_send![&*self.controller, checkForUpdates: None::<&AnyObject>];
        }
    }

    fn set_blocked(&self, blocked: bool) {
        let state = self.delegate.ivars();
        if state.blocked.replace(blocked) && !blocked && state.pending_relaunch.borrow().is_some() {
            // SAFETY: the selector is defined on the delegate class above and takes no argument.
            unsafe {
                self.delegate.performSelectorOnMainThread_withObject_waitUntilDone(
                    sel!(orbitResumePendingRelaunch),
                    None,
                    false,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ptr;
    use std::rc::Rc;

    use super::*;

    /// Drives the delegate through the Objective-C runtime exactly as Sparkle does, so a wrong
    /// selector, BOOL conversion or error out-parameter shows up here rather than mid-update.
    #[test]
    fn blocked_delegate_refuses_updates_and_holds_relaunch_until_unblocked() {
        // SAFETY: test threads are not the main thread, but the delegate only touches its own
        // ivars and Foundation value types; no AppKit or Sparkle object is involved.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let delegate = UpdaterDelegate::new(mtm);
        let updater = NSObject::new();
        let item = NSObject::new();
        let relaunched = Rc::new(Cell::new(false));
        let handler = RcBlock::new({
            let relaunched = Rc::clone(&relaunched);
            move || relaunched.set(true)
        });

        objc2::rc::autoreleasepool(|_| {
            delegate.ivars().blocked.set(true);
            let mut error: *mut NSError = ptr::null_mut();
            // SAFETY: arguments match `-updater:mayPerformUpdateCheck:error:`.
            let allowed: bool = unsafe {
                msg_send![&*delegate, updater: &*updater, mayPerformUpdateCheck: 0 as NSInteger, error: &mut error]
            };
            assert!(!allowed);
            // SAFETY: the delegate stored an autoreleased NSError, alive until the pool drains.
            let error = unsafe { Retained::retain(error) }.expect("an error explains the refusal");
            assert_eq!(error.domain().to_string(), "OrbitDisk.Updates");

            // SAFETY: arguments match `-updater:shouldPostponeRelaunchForUpdate:untilInvokingBlock:`.
            let postponed: bool = unsafe {
                msg_send![&*delegate, updater: &*updater, shouldPostponeRelaunchForUpdate: &*item, untilInvokingBlock: &*handler]
            };
            assert!(postponed);
            // SAFETY: the resume selector takes no arguments.
            unsafe {
                let () = msg_send![&*delegate, orbitResumePendingRelaunch];
            }
            assert!(!relaunched.get(), "relaunch must wait while a removal runs");

            delegate.ivars().blocked.set(false);
            // SAFETY: as above.
            let allowed: bool = unsafe {
                msg_send![&*delegate, updater: &*updater, shouldProceedWithUpdate: &*item, updateCheck: 0 as NSInteger, error: ptr::null_mut::<*mut NSError>()]
            };
            assert!(allowed);
            // SAFETY: as above.
            unsafe {
                let () = msg_send![&*delegate, orbitResumePendingRelaunch];
            }
            assert!(relaunched.get());
            assert!(delegate.ivars().pending_relaunch.borrow().is_none());
        });
    }
}
