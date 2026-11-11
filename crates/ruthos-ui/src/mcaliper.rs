//! Calipers on an M-mode strip.
//!
//! Most M-mode measurements are a depth (straight down) or a time (straight
//! across), so a line drawn there is held to the axis it sets off along. It
//! lets go once the drag turns past 45 degrees from that axis, and is then
//! free for the rest of the drag, measuring depth and time together; Ctrl
//! frees it from the start. A held line is exactly vertical or exactly
//! horizontal, so what it measures is read from its shape and nothing more
//! is stored with it.

use egui::{Color32, Painter, Pos2, Stroke, Vec2};
use ruthos_dicom::meta::UsRegion;
use ruthos_dicom::Frame;
use ruthos_render::ViewTransform;

/// How far, in screen points, the pointer must go before the axis is chosen,
/// so that a shaky press does not choose it.
const DECIDE_AFTER: f32 = 6.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AxisLock {
    /// Not moved far enough to tell.
    #[default]
    Undecided,
    /// Held vertical: a depth.
    Depth,
    /// Held horizontal: a time.
    Time,
    /// Let go: depth and time together.
    Free,
}

impl AxisLock {
    /// The lock an existing line is in, from its shape: for moving one end.
    pub fn of_line(a: Vec2, b: Vec2) -> Self {
        if a.x == b.x {
            AxisLock::Depth
        } else if a.y == b.y {
            AxisLock::Time
        } else {
            AxisLock::Free
        }
    }
}

/// The M-mode strip under an image point: time across, depth down.
pub fn mmode_region(frame: &Frame, p: Vec2) -> Option<&UsRegion> {
    frame
        .meta
        .us_region_at(p.x as f64, p.y as f64)
        .filter(|r| r.is_time_across() && !r.is_velocity_y())
}

/// Where the moving end of a line goes, held to its axis. `anchor` is the
/// end that stays; `raw` is under the pointer; `zoom` is screen points per
/// image pixel, so the choice is made by what is seen.
pub fn hold(lock: &mut AxisLock, anchor: Vec2, raw: Vec2, zoom: f32, free: bool) -> Vec2 {
    if free {
        *lock = AxisLock::Free;
    }
    let d = raw - anchor;
    let (across, down) = (d.x.abs(), d.y.abs());
    match *lock {
        AxisLock::Undecided if d.length() * zoom >= DECIDE_AFTER => {
            *lock = if down >= across {
                AxisLock::Depth
            } else {
                AxisLock::Time
            };
        }
        // Past 45 degrees from the axis it set off along: free.
        AxisLock::Depth if across > down => *lock = AxisLock::Free,
        AxisLock::Time if down > across => *lock = AxisLock::Free,
        _ => {}
    }
    match *lock {
        AxisLock::Depth => Vec2::new(anchor.x, raw.y),
        AxisLock::Time => Vec2::new(raw.x, anchor.y),
        AxisLock::Undecided | AxisLock::Free => raw,
    }
}

fn dashed(painter: &Painter, a: Pos2, b: Pos2, colour: Color32) {
    painter.add(egui::Shape::dashed_line(
        &[a, b],
        Stroke::new(1.0, colour.gamma_multiply(0.75)),
        5.0,
        4.0,
    ));
}

/// The region's corners on screen, image x0..x1 by y0..y1.
fn span(t: &ViewTransform, r: &UsRegion, x: Option<f32>, y: Option<f32>) -> Option<(Pos2, Pos2)> {
    let (x0, x1) = (r.x0 as f32, r.x1 as f32 + 1.0);
    let (y0, y1) = (r.y0 as f32, r.y1 as f32 + 1.0);
    match (x, y) {
        (None, Some(y)) => Some((t.image_to_screen(Vec2::new(x0, y)), t.image_to_screen(Vec2::new(x1, y)))),
        (Some(x), None) => Some((t.image_to_screen(Vec2::new(x, y0)), t.image_to_screen(Vec2::new(x, y1)))),
        _ => None,
    }
}

/// Dotted lines across the whole strip from the ends of a line: across at
/// each end's depth when it measures depth, down at each end's time when it
/// measures time, so the same depth or moment can be read in other beats.
pub fn draw_guides(painter: &Painter, t: &ViewTransform, frame: &Frame, a: Vec2, b: Vec2, colour: Color32) {
    let Some(r) = mmode_region(frame, a) else { return };
    let painter = painter.with_clip_rect(t.rect);
    for end in [a, b] {
        if a.y != b.y {
            if let Some((p, q)) = span(t, r, None, Some(end.y)) {
                dashed(&painter, p, q, colour);
            }
        }
        if a.x != b.x {
            if let Some((p, q)) = span(t, r, Some(end.x), None) {
                dashed(&painter, p, q, colour);
            }
        }
    }
}

/// Crosshairs through the pointer across the whole strip.
pub fn draw_crosshairs(painter: &Painter, t: &ViewTransform, frame: &Frame, at: Vec2, colour: Color32) {
    let Some(r) = mmode_region(frame, at) else { return };
    let painter = painter.with_clip_rect(t.rect);
    for (x, y) in [(None, Some(at.y)), (Some(at.x), None)] {
        if let Some((p, q)) = span(t, r, x, y) {
            dashed(&painter, p, q, colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drag(path: &[(f32, f32)], free: bool) -> (AxisLock, Vec2) {
        let anchor = Vec2::ZERO;
        let mut lock = AxisLock::Undecided;
        let mut at = anchor;
        for &(x, y) in path {
            at = hold(&mut lock, anchor, Vec2::new(x, y), 1.0, free);
        }
        (lock, at)
    }

    #[test]
    fn a_drag_mostly_down_is_held_to_depth() {
        let (lock, at) = drag(&[(1.0, 8.0), (5.0, 30.0), (20.0, 40.0)], false);
        assert_eq!(lock, AxisLock::Depth);
        assert_eq!(at, Vec2::new(0.0, 40.0), "the drift across is dropped");
    }

    #[test]
    fn a_drag_mostly_across_is_held_to_time() {
        let (lock, at) = drag(&[(8.0, 1.0), (40.0, -10.0)], false);
        assert_eq!(lock, AxisLock::Time);
        assert_eq!(at, Vec2::new(40.0, 0.0));
    }

    #[test]
    fn past_45_degrees_it_lets_go_and_stays_free() {
        let (lock, at) = drag(&[(0.0, 10.0), (30.0, 20.0)], false);
        assert_eq!(lock, AxisLock::Free);
        assert_eq!(at, Vec2::new(30.0, 20.0));
        // Coming back towards the axis does not hold it again.
        let (lock, at) = drag(&[(0.0, 10.0), (30.0, 20.0), (2.0, 40.0)], false);
        assert_eq!(lock, AxisLock::Free);
        assert_eq!(at, Vec2::new(2.0, 40.0));
        // A time lets go the same way.
        let (lock, _) = drag(&[(10.0, 0.0), (10.0, 12.0)], false);
        assert_eq!(lock, AxisLock::Free);
    }

    #[test]
    fn a_shaky_start_does_not_choose() {
        let (lock, at) = drag(&[(3.0, 1.0)], false);
        assert_eq!(lock, AxisLock::Undecided);
        assert_eq!(at, Vec2::new(3.0, 1.0));
        // Zoomed in, the same image distance is far enough on screen.
        let mut lock = AxisLock::Undecided;
        hold(&mut lock, Vec2::ZERO, Vec2::new(3.0, 1.0), 4.0, false);
        assert_eq!(lock, AxisLock::Time);
    }

    #[test]
    fn ctrl_frees_it_from_the_start() {
        let (lock, at) = drag(&[(1.0, 8.0), (3.0, 30.0)], true);
        assert_eq!(lock, AxisLock::Free);
        assert_eq!(at, Vec2::new(3.0, 30.0));
    }

    #[test]
    fn a_line_shows_its_lock_by_its_shape() {
        assert_eq!(
            AxisLock::of_line(Vec2::new(5.0, 1.0), Vec2::new(5.0, 9.0)),
            AxisLock::Depth
        );
        assert_eq!(
            AxisLock::of_line(Vec2::new(1.0, 5.0), Vec2::new(9.0, 5.0)),
            AxisLock::Time
        );
        assert_eq!(
            AxisLock::of_line(Vec2::new(1.0, 1.0), Vec2::new(9.0, 5.0)),
            AxisLock::Free
        );
    }
}
