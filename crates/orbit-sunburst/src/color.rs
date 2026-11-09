//! Branch color ordinals and the chart palette. Port of Swift `DiskPalette` and the color-index
//! bookkeeping in `DiskModel.prepareFolder`.

use std::hash::Hash;

use crate::hash::FxHashMap;

/// Palette hues (0..1), indexed by `ordinal % 8`.
pub const PALETTE_HUES: [f32; 8] = [0.045, 0.115, 0.46, 0.58, 0.69, 0.94, 0.32, 0.51];
const BASE_SATURATION: f32 = 0.66;
const SATURATION_STEP: f32 = 0.08;
const MIN_SATURATION: f32 = 0.42;
const BRIGHTNESS: f32 = 0.96;
const GROUP_RGB: [f32; 3] = [0.52, 0.55, 0.61];

/// Stable per-folder branch ordinals: children keep their ordinal across updates and new children
/// take the next unused ordinal, so a branch keeps its hue while sizes reorder the siblings.
/// Ordinals of removed children are retained, so a child that reappears gets its old hue back.
#[derive(Clone, Debug)]
pub struct ColorOrdinals<Id> {
    ordinals: FxHashMap<Id, u32>,
    next: u32,
}

impl<Id> Default for ColorOrdinals<Id> {
    fn default() -> Self {
        Self { ordinals: FxHashMap::default(), next: 0 }
    }
}

impl<Id: Copy + Eq + Hash> ColorOrdinals<Id> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ordinals for a freshly opened folder: the first occurrence of each child gets its index
    /// among distinct children.
    pub fn for_children(children: &[Id]) -> Self {
        let mut ordinals = Self::default();
        ordinals.ordinals.reserve(children.len());
        ordinals.extend(children);
        ordinals
    }

    /// Assigns the next free ordinal to every child not seen before, keeping existing ordinals.
    /// Returns whether anything was added (the caller's color mapping revision changes then).
    pub fn extend(&mut self, children: &[Id]) -> bool {
        let before = self.ordinals.len();
        for &child in children {
            self.ordinals.entry(child).or_insert_with(|| {
                let ordinal = self.next;
                self.next = self.next.saturating_add(1);
                ordinal
            });
        }
        self.ordinals.len() != before
    }

    pub fn get(&self, id: Id) -> Option<u32> {
        self.ordinals.get(&id).copied()
    }

    pub fn len(&self) -> usize {
        self.ordinals.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ordinals.is_empty()
    }
}

/// Hue, saturation, lightness and alpha, each in `0..=1` (GPUI's `Hsla` convention).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsla {
    pub h: f32,
    pub s: f32,
    pub l: f32,
    pub a: f32,
}

/// Paint key for a segment. Segments with equal keys paint with the same color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SegmentColor {
    pub hue_index: u8,
    pub depth: u8,
    pub is_group: bool,
}

impl SegmentColor {
    pub fn branch(ordinal: u32, depth: u8) -> Self {
        Self { hue_index: (ordinal % PALETTE_HUES.len() as u32) as u8, depth, is_group: false }
    }

    pub const GROUP: Self = Self { hue_index: 0, depth: 0, is_group: true };

    /// Branch hues are HSB(hue, max(0.42, 0.66 − 0.08·depth), 0.96); grouped segments are a neutral gray.
    pub fn hsla(self) -> Hsla {
        if self.is_group {
            return rgb_to_hsla(GROUP_RGB);
        }
        let saturation = (BASE_SATURATION - f32::from(self.depth) * SATURATION_STEP).max(MIN_SATURATION);
        hsv_to_hsla(PALETTE_HUES[usize::from(self.hue_index) % PALETTE_HUES.len()], saturation, BRIGHTNESS)
    }
}

fn hsv_to_hsla(h: f32, s: f32, v: f32) -> Hsla {
    let l = v * (1.0 - s / 2.0);
    let s = if l <= 0.0 || l >= 1.0 { 0.0 } else { (v - l) / l.min(1.0 - l) };
    Hsla { h, s, l, a: 1.0 }
}

fn rgb_to_hsla([r, g, b]: [f32; 3]) -> Hsla {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return Hsla { h: 0.0, s: 0.0, l, a: 1.0 };
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    Hsla { h: h / 6.0, s, l, a: 1.0 }
}
