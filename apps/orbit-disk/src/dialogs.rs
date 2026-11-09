//! App-menu dialogs: AI Integration… and Clear Cache… (Swift `AutomationIntegration`,
//! `OrbitDiskApp.clearCache`, `MappingCacheReset`).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui::{AnyWindowHandle, App, ClipboardItem, PromptLevel};
use orbit_automation::installer::IntegrationInstaller;
use orbit_platform::{AppDirs, Shell};
use orbit_ui::Host;
use orbit_ui::lifecycle;
use orbit_ui::settings::SettingsStore;

/// Shows a platform alert on `window`; returns the chosen answer's index.
fn prompt(
    window: AnyWindowHandle,
    level: PromptLevel,
    message: String,
    detail: Option<String>,
    answers: &'static [&'static str],
    cx: &mut App,
) -> Option<futures::channel::oneshot::Receiver<usize>> {
    window.update(cx, |_, window, cx| window.prompt(level, &message, detail.as_deref(), answers, cx)).ok()
}

fn inform(window: AnyWindowHandle, message: &str, detail: &str, cx: &mut App) {
    if let Some(answer) = prompt(window, PromptLevel::Info, message.into(), Some(detail.into()), &["OK"], cx) {
        cx.background_executor().spawn(async move { answer.await.ok() }).detach();
    }
}

fn control_tool() -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_default();
    executable.parent().unwrap_or(Path::new("/")).join("OrbitControl")
}

/// "Connect your AI assistant": Codex, MCP settings, or the command line tool.
pub fn ai_integration(window: AnyWindowHandle, cx: &mut App) {
    let Some(answer) = prompt(
        window,
        PromptLevel::Info,
        "Connect your AI assistant".into(),
        Some(
            "Let Codex use your maps and collection in a new chat. For another assistant, copy the MCP settings. \
             Your assistant must ask you to approve the exact files before removing them."
                .into(),
        ),
        &["Connect to Codex", "Copy MCP Settings", "Install Command Line Tool", "Cancel"],
        cx,
    ) else {
        return;
    };
    cx.spawn(async move |cx| {
        let Ok(choice) = answer.await else { return };
        let installer = IntegrationInstaller::for_current_user(control_tool());
        let (title_ok, result) = match choice {
            0 => (
                "Orbit Disk is connected",
                cx.background_executor().spawn(async move { installer.connect_codex() }).await,
            ),
            1 => match installer.mcp_settings() {
                Ok(settings) => {
                    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(settings))).ok();
                    (
                        "MCP settings copied",
                        Ok("Add the orbit-disk entry to your AI client’s MCP settings, then reconnect that client."
                            .into()),
                    )
                }
                Err(error) => ("", Err(error)),
            },
            2 => (
                "Command line tool installed",
                cx.background_executor().spawn(async move { installer.install_command_line_tool() }).await,
            ),
            _ => return,
        };
        cx.update(|cx| match result {
            Ok(detail) => inform(window, title_ok, &detail, cx),
            Err(error) => inform(window, "Setup couldn’t finish", &error.to_string(), cx),
        })
        .ok();
    })
    .detach();
}

/// "Clear cached maps?": flags the reset, relaunches and quits (refused while removing).
pub fn clear_cache(window: AnyWindowHandle, host: Host, shell: Rc<dyn Shell>, cx: &mut App) {
    if host.state(cx).removal_active() {
        return;
    }
    let Some(answer) = prompt(
        window,
        PromptLevel::Warning,
        "Clear cached maps?".into(),
        Some(
            "Orbit Disk will restart. Saved maps and the current collection will be cleared. \
             Your files and Places will stay unchanged."
                .into(),
        ),
        &["Clear Cache and Restart", "Cancel"],
        cx,
    ) else {
        return;
    };
    cx.spawn(async move |cx| {
        if answer.await != Ok(0) {
            return;
        }
        cx.update(|cx| {
            if host.state(cx).removal_active() {
                return;
            }
            let saved = host.update(cx, |host, _| {
                let store = host.settings_mut();
                store.update(|settings| settings.clear_cache_on_launch = true);
                store.save_now()
            });
            let relaunch = saved.and_then(|()| shell.relaunch());
            match relaunch {
                Ok(handle) => {
                    if lifecycle::may_quit(&host, cx) {
                        // Dropping the handle leaves the relaunch armed.
                        drop(handle);
                        cx.quit();
                    } else {
                        handle.cancel();
                    }
                }
                Err(error) => host.update(cx, |host, cx| {
                    host.set_notice(Some(format!("Couldn’t restart Orbit Disk: {error}").into()), cx)
                }),
            }
        })
        .ok();
    })
    .detach();
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
