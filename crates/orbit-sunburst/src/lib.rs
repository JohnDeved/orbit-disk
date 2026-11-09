//! Sunburst layout, color ordinals and analytic hit-testing. Pure math, no UI dependency.
//!
//! - [`Layout::build`] lays out up to four rings from any [`LayoutSource`] (an arena, typically).
//! - [`Metrics`] places the chart in a view; [`Sector`] gives the exact painted shape of a segment
//!   as arc parameters, a flattened polygon, clipped hatch lines and a badge position.
//! - [`Layout::hit`] resolves a pointer position analytically in O(log n) without allocating.
//! - [`ColorOrdinals`] keeps branch hues stable across folder updates; [`SegmentColor`] maps to HSLA.
//! - [`PresentationCache`] reuses the layout when only collection states change.

mod color;
mod geometry;
mod hash;
mod layout;
mod presentation;

pub use color::{ColorOrdinals, Hsla, PALETTE_HUES, SegmentColor};
pub use geometry::{
    ArcParams, GAP_ARC_LENGTH, GAP_SWEEP_FRACTION, HATCH_SPACING, HOLE_FRACTION, LineSegment, Metrics, OUTER_MARGIN,
    Point, RING_COUNT, RING_INSET, Rect, Sector,
};
pub use layout::{
    Group, Hit, Layout, LayoutSource, MAX_SEGMENTS, MAX_VISIBLE_CHILDREN, MIN_SEGMENT_ANGLE, Members, Segment,
    SegmentIndex,
};
pub use presentation::{CollectionState, Presentation, PresentationCache};

#[cfg(test)]
mod tests;
