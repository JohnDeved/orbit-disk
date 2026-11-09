//! Lets every window be prepared by the platform shell (e.g. color management) without the UI
//! crate knowing about the platform.

use std::rc::Rc;

use gpui::{App, Global, Window};
use orbit_platform::Shell;
use raw_window_handle::HasWindowHandle;

/// The app's shell, installed once at launch.
pub struct ShellGlobal(pub Rc<dyn Shell>);

impl Global for ShellGlobal {}

pub fn install(shell: Rc<dyn Shell>, cx: &mut App) {
    cx.set_global(ShellGlobal(shell));
}

/// Hands a freshly created window to the shell.
pub fn prepare(window: &Window, cx: &App) {
    let Some(ShellGlobal(shell)) = cx.try_global::<ShellGlobal>() else { return };
    if let Ok(handle) = HasWindowHandle::window_handle(window) {
        shell.prepare_window(handle.as_raw());
    }
}
