//! Small motion helpers shared by the footer, the tray and the overlay layer: an eased value
//! that can be retargeted mid-flight, easing curves and interpolation. Views that use them call
//! `window.request_animation_frame()` while [`Transition::running`] and honour Reduce Motion
//! (`theme::reduce_motion`) by jumping instead.

use std::time::{Duration, Instant};

use gpui::{Bounds, Hsla, Pixels, point, px, size};

/// An eased transition of one value (a share, an opacity).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transition {
    from: f32,
    to: f32,
    start: Instant,
    duration: Duration,
}

impl Transition {
    pub fn settled(value: f32, duration: Duration, now: Instant) -> Self {
        Self { from: value, to: value, start: now, duration }
    }

    pub fn value(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.start).as_secs_f32();
        let t = if self.duration.is_zero() { 1.0 } else { elapsed / self.duration.as_secs_f32() };
        self.from + (self.to - self.from) * ease_out(t)
    }

    pub fn target(&self) -> f32 {
        self.to
    }

    pub fn running(&self, now: Instant) -> bool {
        self.from != self.to && now.saturating_duration_since(self.start) < self.duration
    }

    /// Moves towards `to` from the value shown now (jumps when `animate` is false).
    pub fn retarget(&mut self, to: f32, animate: bool, now: Instant) {
        if to == self.to {
            return;
        }
        self.from = if animate { self.value(now) } else { to };
        self.to = to;
        self.start = now;
    }
}

/// A damped spring (SwiftUI's `.spring(response:dampingFraction:)`): mass 1, starting at rest.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// Period of the undamped oscillation, in seconds.
    pub response: f32,
    /// 1 is critically damped; below 1 overshoots a little.
    pub damping: f32,
}

impl Spring {
    /// A released drag settling back into place: quick, with a slight overshoot.
    pub const RETURN: Spring = Spring { response: 0.42, damping: 0.72 };

    fn omega(&self) -> f32 {
        std::f32::consts::TAU / self.response.max(0.01)
    }

    /// The share of the starting displacement left after `t` seconds (1 at the start, settling
    /// to 0, slightly below 0 while it overshoots).
    pub fn remaining(&self, t: f32) -> f32 {
        let (w, z) = (self.omega(), self.damping.clamp(0.05, 1.0));
        let t = t.max(0.0);
        if z >= 1.0 {
            return (1.0 + w * t) * (-w * t).exp();
        }
        let wd = w * (1.0 - z * z).sqrt();
        (-z * w * t).exp() * ((wd * t).cos() + (z * w / wd) * (wd * t).sin())
    }

    /// Seconds until a displacement of `distance` points stays within a quarter point.
    pub fn settle_time(&self, distance: f32) -> f32 {
        let (w, z) = (self.omega(), self.damping.clamp(0.05, 1.0));
        let amplitude = if z >= 1.0 { 2.0 } else { (1.0 + (z / (1.0 - z * z).sqrt()).powi(2)).sqrt() };
        ((distance.max(0.25) * amplitude / 0.25).ln() / (z * w)).max(0.15)
    }
}

/// `t` in 0..=1 eased out (cubic), like SwiftUI's `.easeOut`.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// `t` in 0..=1 eased in and out (cubic), like SwiftUI's `.easeInOut`.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { 4.0 * t * t * t } else { 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0 }
}

/// Elapsed share of `duration` since `start`, clamped to 0..=1.
pub fn progress(start: Instant, duration: Duration, now: Instant) -> f32 {
    if duration.is_zero() {
        return 1.0;
    }
    (now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

pub fn lerp_bounds(a: Bounds<Pixels>, b: Bounds<Pixels>, t: f32) -> Bounds<Pixels> {
    let f = |a: Pixels, b: Pixels| px(lerp(f32::from(a), f32::from(b), t));
    Bounds {
        origin: point(f(a.origin.x, b.origin.x), f(a.origin.y, b.origin.y)),
        size: size(f(a.size.width, b.size.width), f(a.size.height, b.size.height)),
    }
}

pub fn lerp_color(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let (a, b) = (a.to_rgb(), b.to_rgb());
    gpui::Rgba { r: lerp(a.r, b.r, t), g: lerp(a.g, b.g, t), b: lerp(a.b, b.b, t), a: lerp(a.a, b.a, t) }.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transitions_ease_and_retarget_from_the_shown_value() {
        let start = Instant::now();
        let mut t = Transition::settled(0.0, Duration::from_millis(100), start);
        assert!(!t.running(start));
        t.retarget(1.0, true, start);
        let mid = start + Duration::from_millis(50);
        let shown = t.value(mid);
        assert!(shown > 0.5 && shown < 1.0, "ease-out is past half at half time: {shown}");
        t.retarget(0.0, true, mid);
        assert_eq!(t.value(mid), shown, "retargeting continues from the shown value");
        assert_eq!(t.value(mid + Duration::from_millis(200)), 0.0);
        t.retarget(1.0, false, mid);
        assert_eq!(t.value(mid), 1.0, "without motion it jumps");
    }

    #[test]
    fn the_spring_overshoots_slightly_and_settles_in_time() {
        let spring = Spring::RETURN;
        assert_eq!(spring.remaining(0.0), 1.0);
        let samples: Vec<f32> = (0..200).map(|i| spring.remaining(i as f32 * 0.005)).collect();
        let lowest = samples.iter().copied().fold(f32::MAX, f32::min);
        assert!(lowest < 0.0 && lowest > -0.1, "a slight overshoot: {lowest}");
        let settle = spring.settle_time(400.0);
        assert!(settle > 0.3 && settle < 1.0, "{settle}");
        assert!((spring.remaining(settle) * 400.0).abs() <= 0.25, "settled within a quarter point");
    }

    #[test]
    fn curves_and_interpolation_hit_their_ends() {
        for f in [ease_out, ease_in_out] {
            assert_eq!((f(0.0), f(1.0)), (0.0, 1.0));
        }
        assert_eq!(ease_in_out(0.5), 0.5);
        let a = Bounds { origin: point(px(0.), px(10.)), size: size(px(100.), px(50.)) };
        let b = Bounds { origin: point(px(100.), px(30.)), size: size(px(20.), px(10.)) };
        assert_eq!(lerp_bounds(a, b, 0.5), Bounds { origin: point(px(50.), px(20.)), size: size(px(60.), px(30.)) });
    }
}
