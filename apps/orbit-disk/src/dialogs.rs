//! App-menu dialogs: AI Integration… and Clear Cache… (Swift `AutomationIntegration`,
//! `OrbitDiskApp.clearCache`, `MappingCacheReset`).

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{AnyWindowHandle, App, ClipboardItem, PromptLevel};
use orbit_automation::installer::IntegrationInstaller;
use orbit_platform::{AppDirs, Shell};
use orbit_ui::Host;
use orbit_ui::lifecycle;
use orbit_ui::settings::SettingsStore;
use orbit_ui::sheets::{self, Alert, AlertRole};

/// Shows `alert` inside `window` (a main window: outside clicks and Escape cancel it, it waits
/// behind earlier alerts), or as a platform prompt on any other window.
fn show(window: AnyWindowHandle, alert: Alert, level: PromptLevel, cx: &mut App) {
    if let Err(alert) = sheets::present_alert(window, alert, cx) {
        alert.prompt(window, level, cx);
    }
}

fn inform(window: AnyWindowHandle, message: &str, detail: &str, cx: &mut App) {
    show(window, Alert::ok(message.to_owned(), detail.to_owned()), PromptLevel::Info, cx);
}

fn control_tool() -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_default();
    executable.parent().unwrap_or(Path::new("/")).join("OrbitControl")
}

thread_local! {
    /// Connecting Codex or installing the tool is running (Swift `AutomationIntegration.working`).
    static SETUP_RUNNING: Cell<bool> = const { Cell::new(false) };
}

/// "Connect your AI assistant": Codex, MCP settings, or the command line tool.
pub fn ai_integration(window: AnyWindowHandle, cx: &mut App) {
    if SETUP_RUNNING.with(Cell::get) {
        inform(window, "Setup is in progress", "Wait for the current setup to finish.", cx);
        return;
    }
    let alert = Alert::new(
        "Connect your AI assistant",
        "Let Codex use your maps and collection in a new chat. For another assistant, copy the MCP settings. \
         Your assistant must ask you to approve the exact files before removing them.",
    )
    .button("Connect to Codex", AlertRole::Default)
    .button("Copy MCP Settings", AlertRole::Normal)
    .button("Install Command Line Tool", AlertRole::Normal)
    .button("Cancel", AlertRole::Cancel)
    .on_choice(move |choice, cx| run_integration(window, choice, cx));
    show(window, alert, PromptLevel::Info, cx);
}

fn run_integration(window: AnyWindowHandle, choice: usize, cx: &mut App) {
    let installer = IntegrationInstaller::for_current_user(control_tool());
    match choice {
        1 => match installer.mcp_settings() {
            Ok(settings) => {
                cx.write_to_clipboard(ClipboardItem::new_string(settings));
                inform(
                    window,
                    "MCP settings copied",
                    "Add the orbit-disk entry to your AI client’s MCP settings, then reconnect that client.",
                    cx,
                );
            }
            Err(error) => inform(window, "Couldn’t copy settings", &error.to_string(), cx),
        },
        0 | 2 => {
            if SETUP_RUNNING.with(Cell::get) {
                return;
            }
            SETUP_RUNNING.with(|running| running.set(true));
            let connect = choice == 0;
            cx.spawn(async move |cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move {
                        if connect { installer.connect_codex() } else { installer.install_command_line_tool() }
                    })
                    .await;
                cx.update(|cx| {
                    SETUP_RUNNING.with(|running| running.set(false));
                    match result {
                        Ok(detail) => inform(
                            window,
                            if connect { "Orbit Disk is connected" } else { "Command line tool installed" },
                            &detail,
                            cx,
                        ),
                        Err(error) => inform(window, "Setup couldn’t finish", &error.to_string(), cx),
                    }
                })
                .ok();
            })
            .detach();
        }
        _ => {}
    }
}

/// "Clear cached maps?": flags the reset, relaunches and quits (refused while removing).
pub fn clear_cache(window: AnyWindowHandle, host: Host, shell: Rc<dyn Shell>, cx: &mut App) {
    if host.state(cx).removal_active() {
        return;
    }
    let alert = Alert::new(
        "Clear cached maps?",
        "Orbit Disk will restart. Saved maps and the current collection will be cleared. \
         Your files and Places will stay unchanged.",
    )
    .button("Clear Cache and Restart", AlertRole::Default)
    .button("Cancel", AlertRole::Cancel)
    .on_choice(move |choice, cx| {
        if choice == 0 {
            restart_clearing_cache(window, &host, &*shell, cx);
        }
    });
    show(window, alert, PromptLevel::Warning, cx);
}

fn restart_clearing_cache(window: AnyWindowHandle, host: &Host, shell: &dyn Shell, cx: &mut App) {
    // A removal may have started while the alert was open.
    if host.state(cx).removal_active() {
        return;
    }
    let saved = host.update(cx, |host, _| {
        let store = host.settings_mut();
        store.update(|settings| settings.clear_cache_on_launch = true);
        store.save_now()
    });
    match saved.and_then(|()| shell.relaunch()) {
        Ok(handle) => {
            if lifecycle::may_quit(host, cx) {
                // Dropping the handle leaves the relaunch armed.
                drop(handle);
                cx.quit();
            } else {
                handle.cancel();
            }
        }
        Err(error) => {
            // The flag stays set: the next launch still clears the cache.
            inform(window, "Orbit Disk", &format!("Couldn’t restart Orbit Disk: {error}"), cx);
        }
    }
}

/// At launch, before anything reads the cache: completes a confirmed "Clear Cache…".
///
/// Removes `AppDirs::cache` (rebuildable saved maps and search caches) and forgets the last
/// location. Mapping stages and the removal journal in `support` are kept.
pub fn finish_pending_cache_reset(settings: &mut SettingsStore, dirs: &AppDirs) {
    if !settings.settings().clear_cache_on_launch {
        return;
    }
    match std::fs::symlink_metadata(&dirs.cache) {
        Ok(meta) if meta.is_dir() => {
            if let Err(error) = std::fs::remove_dir_all(&dirs.cache) {
                eprintln!("orbit-disk: couldn't clear {}: {error}", dirs.cache.display());
                return;
            }
        }
        Ok(_) => {
            eprintln!("orbit-disk: {} is not a folder; cache not cleared", dirs.cache.display());
        }
        Err(_) => {}
    }
    settings.update(|settings| {
        settings.clear_cache_on_launch = false;
        settings.last_location = None;
    });
    if let Err(error) = settings.save_now() {
        eprintln!("orbit-disk: couldn't save settings: {error}");
    }
}
