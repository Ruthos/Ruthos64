//! The rotary dial: the Surface Dial, the ASUS Dial and the ASUS DialPad, as
//! a precise one-axis control for stepping frames and slices and for fine
//! adjustments of window width, window level, a measurement point and the
//! loupe (docs/dial.md).
//!
//! This module is the platform-neutral part: the dial's modes, how degrees
//! become steps, acceleration and step sizes, all driven by [`DialEvent`]s
//! so they can be tested anywhere. `windows.rs` turns Windows'
//! RadialController into those events; other platforms have no dial.

use std::time::{Duration, Instant};

use egui::Vec2;

pub use ruthos_core::settings::{DialMode, DialSettings};

use crate::roi::RoiKind;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::DialHost;

/// A dial host that does nothing, where the platform has no dial.
#[cfg(not(windows))]
#[derive(Default)]
pub struct DialHost;

#[cfg(not(windows))]
impl DialHost {
    pub fn pump(&mut self, _ctx: &egui::Context, _settings: &DialSettings) {}
    pub fn drain(&self) -> Vec<(Instant, DialEvent)> {
        Vec::new()
    }
    pub fn last_error(&self) -> Option<String> {
        None
    }
    pub fn select(&self, _mode: DialMode) {}
}

/// What the dial did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DialEvent {
    /// A turn, in degrees; clockwise is positive.
    Rotate(f64),
    /// A press of the dial.
    Press,
    /// A mode chosen from Windows' on-screen dial menu.
    Choose(DialMode),
}

/// Events older than this when they are routed are dropped (a dial turned
/// while no viewer was open, say).
pub const STALE: Duration = Duration::from_secs(2);

/// How long the mode indicator stays after the last press or turn.
pub const INDICATOR: Duration = Duration::from_millis(1500);

/// A fast turn: above this many degrees a second, Navigate accelerates.
pub const FAST_DEG_PER_S: f64 = 180.0;
/// Steps a detent makes in Navigate when turning fast.
pub const FAST_STEPS: i64 = 5;

/// What the image under the dial allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Context {
    pub greyscale: bool,
    pub measurement_selected: bool,
    pub loupe_enabled: bool,
}

/// Whether a mode can act on the current image.
pub fn available(mode: DialMode, cx: Context) -> bool {
    match mode {
        DialMode::Navigate => true,
        DialMode::WindowWidth | DialMode::WindowLevel => cx.greyscale,
        DialMode::MeasurementPoint => cx.measurement_selected,
        DialMode::LoupeMagnification => cx.loupe_enabled,
    }
}

/// The dial's state in one viewer.
#[derive(Debug, Clone)]
pub struct Dial {
    pub mode: DialMode,
    /// Degrees turned but not yet a whole step.
    pending: f64,
    last_turn: Option<Instant>,
    /// When the indicator was last refreshed.
    pub shown: Option<Instant>,
    /// When a measurement point was last moved by the dial, so a run of
    /// detents is one undo step.
    pub last_point_edit: Option<Instant>,
}

impl Dial {
    pub fn new(settings: &DialSettings) -> Self {
        Self {
            mode: if settings.modes.contains(&settings.start_mode) {
                settings.start_mode
            } else {
                settings.modes.first().copied().unwrap_or(DialMode::Navigate)
            },
            pending: 0.0,
            last_turn: None,
            shown: None,
            last_point_edit: None,
        }
    }

    /// Turn degrees into whole steps for the current mode: detents of
    /// `degrees_per_step`, reversed if asked, and in Navigate five at a time
    /// when the turn is fast.
    pub fn steps(&mut self, degrees: f64, now: Instant, settings: &DialSettings) -> i64 {
        let degrees = if settings.reverse { -degrees } else { degrees };
        let fast = match self.last_turn {
            Some(t) => {
                let dt = now.duration_since(t).as_secs_f64();
                dt > 0.0 && degrees.abs() / dt > FAST_DEG_PER_S
            }
            None => false,
        };
        self.last_turn = Some(now);
        // A turn in the other direction starts afresh.
        if self.pending != 0.0 && self.pending.signum() != degrees.signum() {
            self.pending = 0.0;
        }
        self.pending += degrees;
        let detent = settings.degrees_per_step.max(1) as f64;
        // A hair below the detent counts, so float noise in reported angles
        // never loses a step.
        let n = ((self.pending + self.pending.signum() * 1e-9) / detent).trunc();
        self.pending -= n * detent;
        let n = n as i64;
        if self.mode == DialMode::Navigate && settings.accelerate && fast {
            n * FAST_STEPS
        } else {
            n
        }
    }

    /// A press: the next enabled mode that can act, in the order of
    /// `DialMode::ALL`. Stays put when no other mode can.
    pub fn next_mode(&mut self, settings: &DialSettings, cx: Context) -> DialMode {
        let order: Vec<DialMode> = DialMode::ALL
            .into_iter()
            .filter(|m| settings.modes.contains(m))
            .collect();
        if order.is_empty() {
            return self.mode;
        }
        let start = order.iter().position(|m| *m == self.mode).unwrap_or(order.len() - 1);
        for k in 1..=order.len() {
            let m = order[(start + k) % order.len()];
            if available(m, cx) {
                self.mode = m;
                self.pending = 0.0;
                break;
            }
        }
        self.mode
    }
}

/// The range of values an image can store: what width and level steps are
/// proportional to.
pub fn stored_range(bits_stored: u16, rescale_slope: f64, min: f32, max: f32) -> f64 {
    let used = ((max - min).abs() as f64).max(255.0);
    if (1..=32).contains(&bits_stored) {
        let stored = ((1u64 << bits_stored) - 1) as f64 * rescale_slope.abs().max(1e-9);
        // An image that stores many more bits than it uses (a CT written
        // as 16-bit) is held to the range it spans, at least 255, so a
        // detent stays a fine adjustment.
        stored.min(used)
    } else {
        used
    }
}

/// One detent of window width: 1 per cent of the stored range, at least 1.
pub fn width_step(range: f64) -> f32 {
    (range * 0.01).round().max(1.0) as f32
}

/// One detent of window level: 0.5 per cent of the stored range, at least 1.
pub fn level_step(range: f64) -> f32 {
    (range * 0.005).round().max(1.0) as f32
}

/// One detent of a measurement point: a tenth of an image pixel.
pub const POINT_STEP: f32 = 0.1;

/// The direction a measurement point moves along: the line for a length,
/// an arrow or a 2D LV line, vertically for an LV M-mode point, the arm for
/// an angle, otherwise across.
pub fn point_axis(kind: &RoiKind, handle: usize) -> Vec2 {
    let unit = |a: Vec2, b: Vec2| {
        let d = b - a;
        if d.length() > 1e-6 {
            d / d.length()
        } else {
            Vec2::X
        }
    };
    match kind {
        RoiKind::Length { a, b } | RoiKind::Arrow { a, b } => {
            if handle == 0 {
                unit(*b, *a)
            } else {
                unit(*a, *b)
            }
        }
        RoiKind::Lv { mmode: true, .. } => Vec2::Y,
        RoiKind::Lv { p, .. } => unit(p[0], p[3]),
        RoiKind::Angle { a, b, c } => match handle {
            0 => unit(*b, *a),
            2 => unit(*b, *c),
            _ => Vec2::X,
        },
        _ => Vec2::X,
    }
}

/// The next loupe magnification from `list`, up or down, staying at the
/// ends.
pub fn next_magnification(list: &[u32], current: f32, steps: i64) -> f32 {
    if list.is_empty() {
        return current;
    }
    let idx = list
        .iter()
        .position(|m| (*m as f32 - current).abs() < 0.01)
        .unwrap_or_else(|| list.iter().position(|m| *m as f32 > current).unwrap_or(list.len() - 1));
    let next = (idx as i64 + steps).clamp(0, list.len() as i64 - 1) as usize;
    list[next] as f32
}

/// A position after `steps` within `0..count`, stopping at the ends.
pub fn clamp_step(current: usize, steps: i64, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    (current as i64 + steps).clamp(0, count as i64 - 1) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> DialSettings {
        DialSettings::default()
    }

    #[test]
    fn degrees_become_steps_at_the_detent() {
        let s = settings();
        let mut d = Dial::new(&s);
        let t0 = Instant::now();
        // Slow turns: a second apart.
        let at = |k: u64| t0 + Duration::from_secs(k);
        assert_eq!(d.steps(10.0, at(1), &s), 1);
        assert_eq!(d.steps(9.0, at(2), &s), 0);
        let mut d = Dial::new(&s);
        assert_eq!(d.steps(19.0, at(1), &s), 1);
        assert_eq!(d.steps(1.0, at(2), &s), 1, "19 then 1 make two");
        assert_eq!(d.steps(-10.0, at(3), &s), -1);
        let reversed = DialSettings {
            reverse: true,
            ..settings()
        };
        let mut d = Dial::new(&reversed);
        assert_eq!(d.steps(10.0, at(1), &reversed), -1);
    }

    #[test]
    fn only_navigate_accelerates_and_only_when_fast() {
        let s = settings();
        let t0 = Instant::now();
        let mut d = Dial::new(&s);
        d.steps(0.0, t0, &s);
        // 10 degrees in 20 ms is 500 degrees a second.
        assert_eq!(d.steps(10.0, t0 + Duration::from_millis(20), &s), FAST_STEPS);
        // 10 degrees in 100 ms is 100 degrees a second: one step.
        let mut d = Dial::new(&s);
        d.steps(0.0, t0, &s);
        assert_eq!(d.steps(10.0, t0 + Duration::from_millis(100), &s), 1);
        let mut d = Dial::new(&s);
        d.mode = DialMode::WindowWidth;
        d.steps(0.0, t0, &s);
        assert_eq!(d.steps(10.0, t0 + Duration::from_millis(20), &s), 1);
        let off = DialSettings {
            accelerate: false,
            ..settings()
        };
        let mut d = Dial::new(&off);
        d.steps(0.0, t0, &off);
        assert_eq!(d.steps(10.0, t0 + Duration::from_millis(20), &off), 1);
    }

    #[test]
    fn width_and_level_steps_follow_the_stored_range() {
        assert_eq!(width_step(stored_range(8, 1.0, 0.0, 255.0)), 3.0);
        assert_eq!(width_step(stored_range(12, 1.0, -1024.0, 3071.0)), 41.0);
        assert_eq!(level_step(stored_range(8, 1.0, 0.0, 255.0)), 1.0);
        assert_eq!(level_step(stored_range(12, 1.0, -1024.0, 3071.0)), 20.0);
        // Tiny ranges still move by one.
        assert_eq!(width_step(stored_range(1, 1.0, 0.0, 1.0)), 1.0);
        // A CT stored as 16 bits steps by what it spans, not by 65535.
        assert_eq!(width_step(stored_range(16, 1.0, -1000.0, 900.0)), 19.0);
    }

    #[test]
    fn navigation_stops_at_the_ends() {
        assert_eq!(clamp_step(0, -1, 64), 0);
        assert_eq!(clamp_step(63, 1, 64), 63);
        assert_eq!(clamp_step(10, 5, 64), 15);
        assert_eq!(clamp_step(0, 3, 0), 0);
    }

    #[test]
    fn a_press_skips_modes_that_cannot_act() {
        let s = settings();
        let mut d = Dial::new(&s);
        assert_eq!(d.mode, DialMode::Navigate);
        let colour = Context {
            greyscale: false,
            measurement_selected: false,
            loupe_enabled: true,
        };
        assert_eq!(d.next_mode(&s, colour), DialMode::LoupeMagnification);
        assert_eq!(d.next_mode(&s, colour), DialMode::Navigate);
        let grey = Context {
            greyscale: true,
            ..colour
        };
        assert_eq!(d.next_mode(&s, grey), DialMode::WindowWidth);
        assert_eq!(d.next_mode(&s, grey), DialMode::WindowLevel);
        assert_eq!(
            d.next_mode(&s, grey),
            DialMode::LoupeMagnification,
            "no measurement selected"
        );
        let selected = Context {
            measurement_selected: true,
            ..grey
        };
        d.mode = DialMode::WindowLevel;
        assert_eq!(d.next_mode(&s, selected), DialMode::MeasurementPoint);
        // Only the enabled modes cycle.
        let two = DialSettings {
            modes: vec![DialMode::Navigate, DialMode::WindowLevel],
            ..settings()
        };
        let mut d = Dial::new(&two);
        assert_eq!(d.next_mode(&two, grey), DialMode::WindowLevel);
        assert_eq!(d.next_mode(&two, grey), DialMode::Navigate);
    }

    #[test]
    fn points_move_along_their_measurement() {
        let line = RoiKind::Length {
            a: Vec2::ZERO,
            b: Vec2::new(3.0, 4.0),
        };
        assert_eq!(point_axis(&line, 1), Vec2::new(0.6, 0.8));
        assert_eq!(point_axis(&line, 0), Vec2::new(-0.6, -0.8));
        let lv = RoiKind::Lv {
            phase: crate::roi::MmodePhase::Diastole,
            mmode: true,
            p: [Vec2::ZERO, Vec2::Y, Vec2::new(0.0, 2.0), Vec2::new(0.0, 3.0)],
        };
        assert_eq!(point_axis(&lv, 2), Vec2::Y);
    }

    #[test]
    fn the_loupe_steps_through_its_list() {
        let list = [2, 3, 4, 6, 8];
        assert_eq!(next_magnification(&list, 4.0, 1), 6.0);
        assert_eq!(next_magnification(&list, 8.0, 1), 8.0);
        assert_eq!(next_magnification(&list, 2.0, -1), 2.0);
        assert_eq!(
            next_magnification(&list, 5.0, 1),
            8.0,
            "between two: from the next one up"
        );
    }
}
