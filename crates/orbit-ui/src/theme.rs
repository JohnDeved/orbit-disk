//! Design tokens from `DESIGN.md` and the Swift reference (`OrbitStyle`, `FileRowView`,
//! `MappingProgressView`). Dimensions are in points (GPUI pixels are points on macOS).
//!
//! Panels use these constants instead of literals so the layout keeps its reserved sizes.

use std::sync::Arc;

use gpui::{FontFeatures, FontWeight, Hsla, Pixels, Rgba, Styled, px, rgb, rgba};
use orbit_sunburst::SegmentColor;

// ---------------------------------------------------------------------------------------------
// Colors.

/// Explorer canvas (`OrbitStyle.canvas`).
pub const CANVAS: u32 = 0x13151a;
/// Sidebar column (`OrbitStyle.sidebar`); also the collection tray background.
pub const SIDEBAR: u32 = 0x171a20;
/// Contents panel, inputs (`OrbitStyle.panel`).
pub const PANEL: u32 = 0x1b1e25;
/// Secondary text, icons (`OrbitStyle.muted`, "slate").
pub const SLATE: u32 = 0xb0b8c7;
/// Coral accent (`OrbitStyle.accent`): scan/collection actions, selection, indexed share.
pub const CORAL: u32 = 0xff8f66;
/// Ivory: files found so far in the mapping track; the app icon ring.
pub const IVORY: u32 = 0xe9dfcf;
/// Primary text.
pub const TEXT: u32 = 0xffffff;
/// Mapping track background.
pub const TRACK: u32 = 0x353944;
/// Selected row fill (solid warm) — paired with a thick coral inset outline.
pub const ROW_SELECTED: u32 = 0x4a2e29;
/// Hovered row fill (neutral) — paired with a thin slate outline.
pub const ROW_HOVER: u32 = 0x25282f;
/// Hovered row outline.
pub const ROW_HOVER_OUTLINE: u32 = 0x78808f;
/// Dark ink used for collection hatching and text on coral.
pub const INK: u32 = 0x05070a;
/// Map center caption (name) and detail colors.
pub const CENTER_NAME: u32 = 0xc2c7d4;
pub const CENTER_DETAIL: u32 = 0xa7abb8;
/// Group sector outline / neutral marks.
pub const NEUTRAL: u32 = 0x858c9c;

pub fn color(hex: u32) -> Rgba {
    rgb(hex)
}

/// `hex` at `alpha` (0..=1).
pub fn with_alpha(hex: u32, alpha: f32) -> Rgba {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u32;
    rgba((hex << 8) | a)
}

/// White at `alpha`: hairlines (0.06), dividers (0.14), hover washes (0.07).
pub fn white(alpha: f32) -> Rgba {
    with_alpha(0xffffff, alpha)
}

pub const DIVIDER_ALPHA: f32 = 0.06;

/// Chart palette: the color of a branch `ordinal` at ring `depth` (shared by map and list).
pub fn chart(ordinal: u32, depth: u8) -> Hsla {
    segment_color(SegmentColor::branch(ordinal, depth))
}

pub fn segment_color(color: SegmentColor) -> Hsla {
    let c = color.hsla();
    Hsla { h: c.h, s: c.s, l: c.l, a: c.a }
}

// ---------------------------------------------------------------------------------------------
// Window and layout.

pub const WINDOW_DEFAULT: (f32, f32) = (1280., 810.);
pub const WINDOW_MIN: (f32, f32) = (1060., 680.);
/// Height of the unified title bar / toolbar row.
pub const TITLEBAR_HEIGHT: Pixels = px(52.);
/// Traffic-light buttons origin inside the transparent title bar.
pub const TRAFFIC_LIGHTS: (f32, f32) = (20., 18.);
/// Width reserved for the traffic lights before toolbar content starts.
pub const TRAFFIC_LIGHT_AREA: f32 = 80.;
/// Location title/path block: 280 wide, inset 142 after the traffic-light area.
pub const TOOLBAR_TITLE_INSET: f32 = 142.;
pub const TOOLBAR_TITLE_WIDTH: Pixels = px(280.);
pub const TOOLBAR_SEARCH_WIDTH: Pixels = px(210.);
pub const TOOLBAR_SEARCH_HEIGHT: Pixels = px(34.);
pub const TOOLBAR_SPACING: Pixels = px(18.);
pub const SIDEBAR_WIDTH: Pixels = px(210.);
/// Explorer header (Up, breadcrumbs, file count).
pub const BREADCRUMB_HEIGHT: Pixels = px(48.);
pub const CONTENTS_WIDTH: Pixels = px(340.);
pub const INSPECTOR_HEIGHT: Pixels = px(164.);
pub const FOOTER_HEIGHT: Pixels = px(51.);
/// Collection tray content height; the tray adds [`TRAY_MARGIN`] around it.
pub const TRAY_HEIGHT: Pixels = px(80.);
pub const TRAY_MARGIN: (f32, f32) = (20., 12.);
/// Fixed gap between the index status and the collection tray.
pub const TRAY_GAP: Pixels = px(12.);
/// File rows (browsing, search and collection states).
pub const ROW_HEIGHT: Pixels = px(40.);
/// Folder arrow target in rows.
pub const ROW_ARROW: (f32, f32) = (32., 36.);
pub const MAP_METRIC_WIDTH: Pixels = px(130.);
pub const MAPPING_TRACK_HEIGHT: Pixels = px(10.);

// ---------------------------------------------------------------------------------------------
// Spacing and shape.

pub const SPACE_XS: Pixels = px(4.);
pub const SPACE_S: Pixels = px(8.);
pub const SPACE_M: Pixels = px(12.);
pub const SPACE_L: Pixels = px(18.);
pub const SPACE_XL: Pixels = px(24.);
pub const RADIUS_PANEL: Pixels = px(12.);
pub const RADIUS_CARD: Pixels = px(9.);
pub const RADIUS_INPUT: Pixels = px(7.);
pub const RADIUS_ROW: Pixels = px(6.);

// ---------------------------------------------------------------------------------------------
// Typography: the system font; monospaced (tabular) digits for sizes and counts.

pub const FONT_FAMILY: &str = ".SystemUIFont";
pub const TEXT_CAPTION: Pixels = px(10.);
pub const TEXT_SMALL: Pixels = px(11.);
pub const TEXT_BODY: Pixels = px(12.);
pub const TEXT_ROW: Pixels = px(13.);
pub const TEXT_TRAY: Pixels = px(14.);
pub const TEXT_HEADING: Pixels = px(15.);
pub const TEXT_TITLE: Pixels = px(17.);
pub const TEXT_METRIC: Pixels = px(18.);
pub const WEIGHT_MEDIUM: FontWeight = FontWeight::MEDIUM;
pub const WEIGHT_SEMIBOLD: FontWeight = FontWeight::SEMIBOLD;

/// OpenType `tnum`: digits share one width so sizes do not jitter as they change.
pub fn tabular_digits() -> FontFeatures {
    FontFeatures(Arc::new(vec![("tnum".into(), 1)]))
}

/// Applies tabular digits to an element's text.
pub fn monospaced_digits<E: Styled>(mut element: E) -> E {
    element.style().text.get_or_insert_with(Default::default).font_features = Some(tabular_digits());
    element
}

// ---------------------------------------------------------------------------------------------
// Formatting shared by panels.

/// Byte counts like Finder (decimal units): `"1.2 GB"`, `"512 bytes"`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1000 {
        return if bytes == 1 { "1 byte".into() } else { format!("{bytes} bytes") };
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 || unit == 0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `12,345` (grouped thousands).
pub fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Lossy display text for a raw path or name.
pub fn display(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

// --- chrome panels ---
// Motion, footer timings and relative ages used by the toolbar, sidebar, footer, tray and welcome
// panels.

/// The OS "Reduce Motion" preference (`Shell::reduce_motion`), installed once at launch with
/// [`set_reduce_motion`]. Absent (tests, harness) means full motion.
#[derive(Clone, Copy, Debug, Default)]
pub struct Motion {
    pub reduce: bool,
}

impl gpui::Global for Motion {}

pub fn set_reduce_motion(reduce: bool, cx: &mut gpui::App) {
    cx.set_global(Motion { reduce });
}

/// Reduce Motion: no fill transitions, static activity symbols.
pub fn reduce_motion(cx: &gpui::App) -> bool {
    cx.try_global::<Motion>().is_some_and(|motion| motion.reduce)
}

/// Mapping track fill transition (`MappingProgressView`: ease-out 0.35 s).
pub const TRACK_TRANSITION: std::time::Duration = std::time::Duration::from_millis(350);
/// Fade of the counts and track when a map finishes saving (ease-out 0.18 s).
pub const FOOTER_FADE: std::time::Duration = std::time::Duration::from_millis(180);
/// Coarse refresh of "Indexed · updated … ago" while it is visible (minute granularity).
pub const AGE_REFRESH: std::time::Duration = std::time::Duration::from_secs(30);
/// One turn of the activity indicator.
pub const SPINNER_PERIOD: std::time::Duration = std::time::Duration::from_millis(900);
/// Footer content: the 28-point progress block, 3 points, the 20-point status row.
pub const FOOTER_PROGRESS_HEIGHT: Pixels = px(28.);
pub const FOOTER_ROW_HEIGHT: Pixels = px(20.);
/// Footer count groups ("12,345 found") and the action button.
pub const FOOTER_COUNT_WIDTH: Pixels = px(164.);
pub const FOOTER_ACTION_WIDTH: Pixels = px(64.);
pub const FOOTER_ROOT_WIDTH: f32 = 160.;
/// Legacy footer (no progress values): title, count and button columns.
pub const FOOTER_LEGACY_TITLE_WIDTH: Pixels = px(172.);
pub const FOOTER_LEGACY_COUNT_WIDTH: Pixels = px(140.);
pub const FOOTER_LEGACY_ACTION_WIDTH: Pixels = px(130.);
/// Fixed sidebar device area: volume metadata arriving never moves Places.
pub const SIDEBAR_DEVICES_HEIGHT: Pixels = px(100.);
/// Toolbar buttons keep fixed widths so labels and enabled states never shift the title bar.
pub const TOOLBAR_REFRESH_WIDTH: Pixels = px(96.);
pub const TOOLBAR_OPEN_WIDTH: Pixels = px(132.);
/// Breadcrumb row: Up button, opening indicator slot and file count column.
pub const BREADCRUMB_UP_WIDTH: Pixels = px(64.);
pub const BREADCRUMB_COUNT_WIDTH: Pixels = px(138.);

/// `t` in 0..=1 eased out (cubic), like SwiftUI's `.easeOut`.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// The age of a saved map for "Indexed · updated {age} ago", at minute granularity (the footer
/// refreshes it every [`AGE_REFRESH`]): "less than a minute", "5 min", "2 hr, 5 min", "3 days, 4 hr".
pub fn format_age(age: std::time::Duration) -> String {
    let minutes = age.as_secs() / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let (days, hours) = (hours / 24, hours % 24);
    let unit = |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if days > 0 {
        let days_text = unit(days, "day", "days");
        if hours > 0 && days < 7 { format!("{days_text}, {}", unit(hours, "hr", "hr")) } else { days_text }
    } else if hours > 0 {
        if minutes > 0 { format!("{hours} hr, {minutes} min") } else { format!("{hours} hr") }
    } else if minutes > 0 {
        format!("{minutes} min")
    } else {
        "less than a minute".to_owned()
    }
}
