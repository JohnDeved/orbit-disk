//! Orbit Disk: app lifecycle. Chooses the backend, restores settings, creates the shell, menus,
//! windows and the automation server, and guards quitting while files are being removed.

mod args;
mod automation;
mod dialogs;
mod shell;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyWindowHandle, App, Application, WindowHandle, px, size};
use orbit_app_state::{Backend, Config, Event};
use orbit_platform::{Platform, Shell};
use orbit_ui::fixture::{self, FixtureOptions, SharedFixture};
use orbit_ui::host::path_bytes;
use orbit_ui::marketing::{self, MarketingRun};
use orbit_ui::settings::{FIXTURE_SETTINGS_FILE, SETTINGS_FILE, Settings, SettingsStore};
use orbit_ui::settings_window::{self, SettingsWindow};
use orbit_ui::{Assets, Host, HostEvent, HostOptions, actions, lifecycle, main_window};

/// The real orbit-core backend, once the integrator wires it. Until then every run uses the
/// fixture backend (and the fixture settings file).
/// The production engine: orbit-core over this OS's platform layer.
fn real_backend() -> Option<Arc<dyn Backend>> {
    let platform = Arc::new(orbit_native::Native::new());
    Some(Arc::new(orbit_backend::EngineBackend::new(platform)))
}

/// Whether a remembered location still exists as a folder (metadata only; no content reads).
fn still_a_folder(path: &[u8]) -> bool {
    std::fs::metadata(orbit_ui::host::os_path(path)).is_ok_and(|meta| meta.is_dir())
}

fn home() -> Vec<u8> {
    std::env::var_os("HOME").map(|home| path_bytes(home.as_ref())).unwrap_or_else(|| b"/".to_vec())
}

fn id_seed() -> u64 {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
    nanos ^ (u64::from(std::process::id()) << 32)
}

fn main() {
    let args = match args::Args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let platform = orbit_native::Native::new();
    let dirs = platform.app_dirs();
    let real = if args.fixture { None } else { real_backend() };
    let fixture_mode = real.is_none();
    let (backend, home, settings_file): (Arc<dyn Backend>, Vec<u8>, &str) = match real {
        Some(backend) => (backend, home(), SETTINGS_FILE),
        None => {
            let mut options =
                if args.marketing.is_some() { FixtureOptions::sample() } else { FixtureOptions::default() };
            if let Some(entries) = args.fixture_entries {
                options.entries = entries;
            }
            let started = std::time::Instant::now();
            let backend = SharedFixture::new(options);
            eprintln!("orbit-disk: fixture backend, {} entries generated in {:?}", backend.0.len(), started.elapsed());
            (Arc::new(backend), fixture::HOME.to_vec(), FIXTURE_SETTINGS_FILE)
        }
    };
    // Screenshots never read or write saved settings (window, sort, Places, last location).
    let mut settings = match args.marketing {
        Some(_) => SettingsStore::in_memory(Settings::default()),
        None => SettingsStore::load(dirs.settings.join(settings_file)),
    };
    if !fixture_mode {
        dialogs::finish_pending_cache_reset(&mut settings, &dirs);
    }

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        let shell: Rc<dyn Shell> = Rc::new(shell::create());
        // Screenshots run without animations, whatever the OS preference.
        let marketing = args.marketing;
        orbit_ui::native_window::install(Rc::clone(&shell), cx);
        orbit_ui::theme::set_reduce_motion(marketing.is_some() || shell.reduce_motion(), cx);
        orbit_ui::sheets::set_privacy_settings_url(shell.privacy_settings_url(), cx);

        // First launch: import the Swift app's Places and last location.
        if !fixture_mode && !settings.settings().legacy_imported {
            let legacy = shell.import_legacy_settings().unwrap_or_default();
            settings.update(|s| {
                for place in &legacy.places {
                    let place = path_bytes(place);
                    if !s.places.contains(&place) {
                        s.places.push(place);
                    }
                }
                if s.last_location.is_none() {
                    s.last_location = legacy.last_location.as_deref().map(path_bytes);
                }
                s.legacy_imported = true;
            });
            settings.save(cx.background_executor());
        }
        let last_location = match (&args.scan, fixture_mode) {
            _ if marketing.is_some() => Some(fixture::HOME.to_vec()),
            (Some(_), _) => None,
            (None, true) => settings.settings().last_location.clone().or_else(|| Some(fixture::ROOT.to_vec())),
            // A remembered folder that was deleted or moved meanwhile opens the welcome screen
            // instead of greeting the user with a "couldn't be opened" error.
            (None, false) => settings.settings().last_location.clone().filter(|path| still_a_folder(path)),
        };
        let mut config = Config::new(home);
        config.places = settings.settings().places.clone();
        config.compare_names = orbit_core::collate::compare;
        config.id_seed = id_seed();
        let host = Host::new(HostOptions { config, backend, settings }, cx);

        // Updater: blocked while files are removed.
        {
            let shell = Rc::clone(&shell);
            cx.subscribe(host.entity(), move |_, event: &HostEvent, _| match event {
                HostEvent::RemovalActivityChanged(active) => {
                    if let Some(updater) = shell.updater() {
                        updater.set_blocked(*active);
                    }
                }
            })
            .detach();
        }

        cx.bind_keys(actions::key_bindings());
        cx.set_menus(actions::menus());
        register_app_actions(&host, &shell, cx);

        let window = match main_window::open(&host, args.harness.clone(), cx) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("orbit-disk: couldn't open the main window: {error}");
                cx.quit();
                return;
            }
        };
        MAIN_WINDOW.with(|main| *main.borrow_mut() = Some(window.into()));
        if let Some((width, height)) = args.window_size {
            window.update(cx, |_, window, _| window.resize(size(px(width), px(height)))).ok();
        }
        // Reduce Motion may change in System Settings while the app runs: re-read it whenever
        // the main window becomes active (a cheap preference query).
        if marketing.is_none() {
            let shell = Rc::clone(&shell);
            window
                .update(cx, |_, window, cx| {
                    cx.observe_window_activation(window, move |_, window, cx| {
                        if window.is_window_active() {
                            orbit_ui::theme::set_reduce_motion(shell.reduce_motion(), cx);
                        }
                    })
                    .detach();
                })
                .ok();
        }
        cx.activate(true);
        // The main window is on screen: start scheduled update checks (idempotent). Screenshots
        // never check (an update prompt would cover the window).
        if let Some(updater) = shell.updater().filter(|_| marketing.is_none()) {
            updater.start();
        }
        // Closing the main window quits (Swift: terminate after last window closed).
        cx.on_window_closed(|cx| {
            let main_open = MAIN_WINDOW.with(|main| main.borrow().is_some_and(|main| cx.windows().contains(&main)));
            if !main_open {
                cx.quit();
            }
        })
        .detach();

        let server = if args.harness.is_none() && marketing.is_none() {
            automation::start(&host, &dirs.runtime, cx)
        } else {
            None
        };
        {
            let host = host.clone();
            cx.on_app_quit(move |cx| {
                if let Some(server) = &server {
                    server.stop();
                }
                host.update(cx, |host, _| {
                    if let Err(error) = host.settings_mut().save_now() {
                        eprintln!("orbit-disk: couldn't save settings: {error}");
                    }
                });
                async {}
            })
            .detach();
        }

        if args.harness.is_some() {
            // Frames only tick while the display is awake and unlocked; don't hang without them.
            cx.spawn(async move |cx| {
                cx.background_executor().timer(HARNESS_TIMEOUT).await;
                eprintln!("frame-harness: no result after {HARNESS_TIMEOUT:?} (is the screen locked or asleep?)");
                cx.update(|cx| cx.quit()).ok();
            })
            .detach();
        }
        host.update(cx, |host, cx| host.launch(last_location, cx));
        if let Some(path) = args.scan.clone() {
            host.send(Event::OpenLocation { path, force: false }, cx);
        }
        if let Some(state) = marketing {
            let run = MarketingRun {
                state,
                window_size: args.window_size.map(|(width, height)| size(px(width), px(height))),
                await_presented_frame: true,
            };
            marketing::drive(
                window,
                run,
                |_| eprintln!("marketing-state: ready"),
                |message, cx| {
                    eprintln!("marketing-state: {message}");
                    cx.quit();
                },
                cx,
            )
            .detach();
        }
    });
}

/// The live frame harness gives up after this long.
const HARNESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

thread_local! {
    static MAIN_WINDOW: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
    static SETTINGS_WINDOW: RefCell<Option<WindowHandle<SettingsWindow>>> = const { RefCell::new(None) };
}

fn main_window_handle() -> Option<AnyWindowHandle> {
    MAIN_WINDOW.with(|main| *main.borrow())
}

fn register_app_actions(host: &Host, shell: &Rc<dyn Shell>, cx: &mut App) {
    {
        let host = host.clone();
        cx.on_action(move |_: &actions::Quit, cx| lifecycle::quit(&host, cx));
    }
    cx.on_action(|_: &actions::Hide, cx| cx.hide());
    cx.on_action(|_: &actions::HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &actions::ShowAll, cx| cx.unhide_other_apps());
    {
        let (host, shell) = (host.clone(), Rc::clone(shell));
        cx.on_action(move |_: &actions::CheckForUpdates, cx| {
            if host.state(cx).removal_active() {
                return;
            }
            if let Some(updater) = shell.updater().filter(|u| u.state().can_check) {
                updater.check_now();
            }
        });
    }
    {
        let (host, shell) = (host.clone(), Rc::clone(shell));
        cx.on_action(move |_: &actions::OpenSettings, cx| {
            let reused = SETTINGS_WINDOW
                .with(|w| *w.borrow())
                .is_some_and(|window| window.update(cx, |_, window, _| window.activate_window()).is_ok());
            if !reused {
                match settings_window::open(Rc::clone(&shell), host.clone(), cx) {
                    Ok(window) => SETTINGS_WINDOW.with(|w| *w.borrow_mut() = Some(window)),
                    Err(error) => eprintln!("orbit-disk: couldn't open Settings: {error}"),
                }
            }
        });
    }
    cx.on_action(|_: &actions::AiIntegration, cx| {
        if let Some(window) = cx.active_window().or_else(main_window_handle) {
            dialogs::ai_integration(window, cx);
        }
    });
    {
        let (host, shell) = (host.clone(), Rc::clone(shell));
        cx.on_action(move |_: &actions::ClearCache, cx| {
            if let Some(window) = main_window_handle() {
                dialogs::clear_cache(window, host.clone(), Rc::clone(&shell), cx);
            }
        });
    }
}
