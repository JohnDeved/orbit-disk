//! Menu and keyboard actions (`OrbitDiskApp.swift` commands).
//!
//! Window actions (Open Folder…, Refresh, Explore menu, ⌘F) are handled by the main window's
//! root element, registered only while enabled, so GPUI disables their menu items otherwise.
//! App actions (Quit, Settings…, Check for Updates…, AI Integration…, Clear Cache…, Hide) are
//! global handlers the app binary registers.

use gpui::{KeyBinding, Menu, MenuItem, NoAction, OsAction, SystemMenuType, actions};

actions!(
    orbit,
    [
        Quit,
        CheckForUpdates,
        AiIntegration,
        ClearCache,
        OpenSettings,
        Hide,
        HideOthers,
        ShowAll,
        OpenFolder,
        RefreshMap,
        RescanEverything,
        CloseWindow,
        Minimize,
        Zoom,
        GoUp,
        OpenSelectedFolder,
        RevealInFinder,
        QuickLook,
        AddToCollection,
        FocusSearch,
        Cut,
        Copy,
        Paste,
        SelectAll,
        Undo,
        Redo,
    ]
);

/// Key context of the main window root; Space (Quick Look) is bound in it.
pub const WINDOW_CONTEXT: &str = "OrbitWindow";
/// Key context of text inputs: Space types a space there instead of Quick Look.
pub const TEXT_INPUT_CONTEXT: &str = "TextInput";

pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-o", OpenFolder, None),
        KeyBinding::new("cmd-r", RefreshMap, None),
        KeyBinding::new("alt-cmd-r", RescanEverything, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-[", GoUp, None),
        KeyBinding::new("cmd-down", OpenSelectedFolder, None),
        KeyBinding::new("shift-cmd-r", RevealInFinder, None),
        KeyBinding::new("space", QuickLook, Some(WINDOW_CONTEXT)),
        KeyBinding::new("space", NoAction, Some(TEXT_INPUT_CONTEXT)),
        KeyBinding::new("cmd-k", AddToCollection, None),
        KeyBinding::new("cmd-f", FocusSearch, None),
        KeyBinding::new("cmd-x", Cut, Some(TEXT_INPUT_CONTEXT)),
        KeyBinding::new("cmd-c", Copy, Some(TEXT_INPUT_CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(TEXT_INPUT_CONTEXT)),
        KeyBinding::new("cmd-a", SelectAll, Some(TEXT_INPUT_CONTEXT)),
        KeyBinding::new("cmd-z", Undo, Some(TEXT_INPUT_CONTEXT)),
        KeyBinding::new("shift-cmd-z", Redo, Some(TEXT_INPUT_CONTEXT)),
    ]
}

/// The menu bar (`OrbitDiskApp.swift:40-63` plus the standard macOS items).
pub fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Orbit Disk".into(),
            items: vec![
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::action("AI Integration…", AiIntegration),
                MenuItem::action("Clear Cache…", ClearCache),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Orbit Disk", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Orbit Disk", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open Folder…", OpenFolder),
                MenuItem::action("Refresh", RefreshMap),
                MenuItem::action("Rescan Everything", RescanEverything),
                MenuItem::separator(),
                MenuItem::action("Close Window", CloseWindow),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            ],
        },
        Menu {
            name: "Explore".into(),
            items: vec![
                MenuItem::action("Go Up", GoUp),
                MenuItem::action("Open Selected Folder", OpenSelectedFolder),
                MenuItem::action("Reveal in Finder", RevealInFinder),
                MenuItem::action("Quick Look", QuickLook),
                MenuItem::action("Add to Collection", AddToCollection),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![MenuItem::action("Minimize", Minimize), MenuItem::action("Zoom", Zoom)],
        },
    ]
}
