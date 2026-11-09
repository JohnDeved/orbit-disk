//! Icons replacing the SF Symbols of the Swift app, and the embedded [`Assets`] source.
//!
//! The SVGs are [Lucide](https://lucide.dev) icons (ISC, see `assets/icons/LICENSE`), embedded
//! at compile time. `*-fill` variants are derived locally (filled shapes with cut-outs). GPUI
//! paints an SVG as a monochrome mask: color an icon with `text_color`.
//!
//! ```ignore
//! icon(Icon::Folder).size(px(14.)).text_color(theme::ACCENT)
//! ```

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString, Styled, Svg, px, svg};

macro_rules! icons {
    ($($variant:ident => $file:literal, $sf:literal;)*) => {
        /// An embedded icon. The doc string of each variant names the SF Symbol it replaces.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Icon {
            $(#[doc = $sf] $variant,)*
        }

        impl Icon {
            pub const ALL: &[Icon] = &[$(Icon::$variant,)*];

            /// Asset path (`icons/<name>.svg`).
            pub const fn path(self) -> &'static str {
                match self {
                    $(Icon::$variant => concat!("icons/", $file, ".svg"),)*
                }
            }

            /// The SF Symbol this icon stands in for.
            pub const fn sf_symbol(self) -> &'static str {
                match self {
                    $(Icon::$variant => $sf,)*
                }
            }
        }

        static FILES: &[(&str, &[u8])] = &[
            $((concat!("icons/", $file, ".svg"), include_bytes!(concat!("../assets/icons/", $file, ".svg"))),)*
        ];
    };
}

icons! {
    ArrowDown => "arrow-down", "arrow.down";
    ArrowUp => "arrow-up", "arrow.up";
    ArrowUpRight => "arrow-up-right", "arrow.up.forward";
    ChartPie => "chart-pie", "chart.pie";
    Check => "check", "checkmark";
    ChevronRight => "chevron-right", "chevron.right";
    ChevronUp => "chevron-up", "chevron.up";
    CircleArrowDown => "circle-arrow-down", "arrow.down.circle";
    CircleCheck => "circle-check", "checkmark.circle";
    CircleCheckFill => "circle-check-fill", "checkmark.circle.fill";
    CircleMinus => "circle-minus", "minus.circle";
    CircleMinusFill => "circle-minus-fill", "minus.circle.fill";
    CirclePause => "circle-pause", "pause.circle";
    CircleX => "circle-x", "xmark.circle";
    CircleXFill => "circle-x-fill", "xmark.circle.fill";
    Close => "x", "xmark";
    Ellipsis => "ellipsis", "ellipsis";
    Eye => "eye", "eye";
    File => "file", "doc";
    FileFill => "file-fill", "doc.fill";
    FileText => "file-text", "doc.text";
    Folder => "folder", "folder";
    FolderFill => "folder-fill", "folder.fill";
    FolderPlus => "folder-plus", "folder.badge.plus";
    HardDrive => "hard-drive", "internaldrive";
    House => "house", "house";
    Layers => "layers", "square.stack.fill";
    LayoutGrid => "layout-grid", "square.grid.2x2";
    Loader => "loader-circle", "ProgressView (static activity symbol)";
    Pause => "pause", "pause.fill";
    Play => "play", "play.fill";
    Plus => "plus", "plus";
    Pointer => "mouse-pointer-2", "cursorarrow";
    Refresh => "rotate-cw", "arrow.clockwise";
    Search => "search", "magnifyingglass";
    Trash => "trash-2", "trash";
    TrayDown => "import", "tray.and.arrow.down / tray.and.arrow.down.fill";
    TrayFull => "inbox", "tray.full";
    Warning => "triangle-alert", "exclamationmark.triangle";
}

/// A 14 × 14 icon element; size and color it like text (`.size(..)`, `.text_color(..)`).
pub fn icon(icon: Icon) -> Svg {
    svg().path(icon.path()).size(px(14.)).flex_none()
}

/// Compile-time embedded assets (icons). Pass to `Application::with_assets`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(FILES.iter().find(|(name, _)| *name == path).map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(FILES.iter().filter(|(name, _)| name.starts_with(path)).map(|(name, _)| SharedString::from(*name)).collect())
    }
}
