//! Image-to-screen transform.
//!
//! Image space is in pixels with the origin at the top-left corner of the
//! top-left pixel, x to the right, y down. Rows may be taller than columns
//! (anisotropic pixel spacing); `aspect` scales y so millimetres are square on
//! screen. Zoom is screen points per image column.

use egui::{Pos2, Rect, Vec2};

/// Mutable view parameters the user drives with the mouse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    /// Screen points per image pixel (column).
    pub zoom: f32,
    /// Offset of the image centre from the viewport centre, in screen points.
    pub pan: Vec2,
    /// Clockwise rotation in degrees.
    pub rotation_deg: f32,
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    pub window_center: f32,
    pub window_width: f32,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: Vec2::ZERO,
            rotation_deg: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            window_center: 0.0,
            window_width: 1.0,
        }
    }
}

impl ViewState {
    /// Reset geometry (not the window) so the image fits the viewport.
    pub fn fit(&mut self, rect: Rect, image_w: u32, image_h: u32, aspect: f32) {
        self.zoom = ViewTransform::fit_zoom(rect, image_w, image_h, aspect, self.rotation_deg);
        self.pan = Vec2::ZERO;
    }
}

/// Everything needed to map between image and screen space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewTransform {
    pub rect: Rect,
    pub image_w: u32,
    pub image_h: u32,
    /// Row spacing divided by column spacing.
    pub aspect: f32,
    pub state: ViewState,
}

impl ViewTransform {
    pub fn new(rect: Rect, image_w: u32, image_h: u32, aspect: f32, state: ViewState) -> Self {
        Self {
            rect,
            image_w,
            image_h,
            aspect: if aspect.is_finite() && aspect > 0.0 { aspect } else { 1.0 },
            state,
        }
    }

    /// Zoom that fits the whole (possibly rotated) image inside `rect`.
    pub fn fit_zoom(rect: Rect, image_w: u32, image_h: u32, aspect: f32, rotation_deg: f32) -> f32 {
        if image_w == 0 || image_h == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return 1.0;
        }
        let w = image_w as f32;
        let h = image_h as f32 * aspect;
        let (s, c) = rotation_deg.to_radians().sin_cos();
        let rot_w = (w * c).abs() + (h * s).abs();
        let rot_h = (w * s).abs() + (h * c).abs();
        let margin = 0.98;
        (rect.width() * margin / rot_w).min(rect.height() * margin / rot_h)
    }

    fn centre(&self) -> Vec2 {
        Vec2::new(self.image_w as f32 / 2.0, self.image_h as f32 / 2.0)
    }

    /// Map an image-space point (pixels) to screen points.
    pub fn image_to_screen(&self, p: Vec2) -> Pos2 {
        let s = &self.state;
        let mut v = p - self.centre();
        v.y *= self.aspect;
        if s.flip_horizontal {
            v.x = -v.x;
        }
        if s.flip_vertical {
            v.y = -v.y;
        }
        let (sin, cos) = s.rotation_deg.to_radians().sin_cos();
        let r = Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos);
        let scaled = r * s.zoom;
        self.rect.center() + scaled + s.pan
    }

    /// Map a screen point back to image space (pixels, fractional).
    pub fn screen_to_image(&self, p: Pos2) -> Vec2 {
        let s = &self.state;
        let v = (p - self.rect.center() - s.pan) / s.zoom.max(1e-6);
        let (sin, cos) = (-s.rotation_deg).to_radians().sin_cos();
        let mut r = Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos);
        if s.flip_horizontal {
            r.x = -r.x;
        }
        if s.flip_vertical {
            r.y = -r.y;
        }
        r.y /= self.aspect;
        r + self.centre()
    }

    /// The transform of a loupe: this view magnified `magnification` times,
    /// drawn into `rect` with `centre` (an image point) at its middle.
    /// Rotation, flips and aspect are kept so the loupe shows the image the
    /// way the cell does.
    pub fn loupe(&self, centre: Vec2, rect: Rect, magnification: f32) -> ViewTransform {
        let mut t = *self;
        t.rect = rect;
        t.state.zoom = self.state.zoom * magnification.max(1.0);
        t.state.pan = Vec2::ZERO;
        let at_zero_pan = t.image_to_screen(centre);
        t.state.pan = rect.center() - at_zero_pan;
        t
    }

    /// Integer pixel under a screen point, if inside the image.
    pub fn pixel_at(&self, p: Pos2) -> Option<(i64, i64)> {
        let v = self.screen_to_image(p);
        let x = v.x.floor() as i64;
        let y = v.y.floor() as i64;
        if x < 0 || y < 0 || x >= self.image_w as i64 || y >= self.image_h as i64 {
            None
        } else {
            Some((x, y))
        }
    }

    /// Column-major 4x4 matrix taking image pixel coordinates to clip space
    /// within `rect` (which the callback's viewport is set to).
    pub fn image_to_clip_matrix(&self) -> [[f32; 4]; 4] {
        // Build from the affine map by evaluating basis vectors.
        let o = self.image_to_screen(Vec2::ZERO);
        let ex = self.image_to_screen(Vec2::new(1.0, 0.0)) - o;
        let ey = self.image_to_screen(Vec2::new(0.0, 1.0)) - o;
        let half_w = self.rect.width() / 2.0;
        let half_h = self.rect.height() / 2.0;
        let c = self.rect.center();
        // screen -> clip: x' = (x - cx) / half_w ; y' = -(y - cy) / half_h
        let to_clip = |p: Pos2| -> (f32, f32) { ((p.x - c.x) / half_w, -(p.y - c.y) / half_h) };
        let (ox, oy) = to_clip(o);
        let (exx, exy) = ((ex.x) / half_w, -(ex.y) / half_h);
        let (eyx, eyy) = ((ey.x) / half_w, -(ey.y) / half_h);
        // Column-major: columns are the images of basis vectors and the origin.
        [
            [exx, exy, 0.0, 0.0],
            [eyx, eyy, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [ox, oy, 0.0, 1.0],
        ]
    }

    /// Length in millimetres of a segment given in image pixels, when the
    /// pixel spacing (row, column) is known.
    pub fn length_mm(a: Vec2, b: Vec2, spacing: Option<[f64; 2]>) -> Option<f64> {
        let [row_mm, col_mm] = spacing?;
        let dx = (b.x - a.x) as f64 * col_mm;
        let dy = (b.y - a.y) as f64 * row_mm;
        Some((dx * dx + dy * dy).sqrt())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loupe_centres_the_point_and_scales() {
        let rect = Rect::from_min_size(Pos2::new(10.0, 20.0), Vec2::new(400.0, 300.0));
        let mut state = ViewState::default();
        state.zoom = 1.5;
        state.pan = Vec2::new(7.0, -3.0);
        state.rotation_deg = 90.0;
        state.flip_horizontal = true;
        let t = ViewTransform::new(rect, 512, 512, 1.0, state);
        let loupe_rect = Rect::from_min_size(Pos2::new(600.0, 100.0), Vec2::splat(160.0));
        let centre = Vec2::new(123.25, 400.5);
        let l = t.loupe(centre, loupe_rect, 4.0);
        let c = l.image_to_screen(centre);
        assert!((c - loupe_rect.center()).length() < 1e-3);
        // One image pixel is zoom × magnification screen points.
        let d = l.image_to_screen(centre + Vec2::new(1.0, 0.0)) - c;
        assert!((d.length() - 6.0).abs() < 1e-3);
        assert_eq!(l.state.rotation_deg, 90.0);
        assert!(l.state.flip_horizontal);
    }

    fn t() -> ViewTransform {
        let rect = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(800.0, 600.0));
        ViewTransform::new(
            rect,
            400,
            300,
            1.0,
            ViewState {
                zoom: 2.0,
                pan: Vec2::new(10.0, -5.0),
                rotation_deg: 30.0,
                flip_horizontal: true,
                flip_vertical: false,
                ..Default::default()
            },
        )
    }

    #[test]
    fn round_trip() {
        let t = t();
        for p in [Vec2::new(0.0, 0.0), Vec2::new(400.0, 300.0), Vec2::new(123.4, 56.7)] {
            let s = t.image_to_screen(p);
            let back = t.screen_to_image(s);
            assert!((back - p).length() < 1e-3, "{p:?} -> {s:?} -> {back:?}");
        }
    }

    #[test]
    fn centre_maps_to_centre_plus_pan() {
        let t = t();
        let s = t.image_to_screen(Vec2::new(200.0, 150.0));
        assert!((s - (t.rect.center() + t.state.pan)).length() < 1e-4);
    }

    #[test]
    fn matrix_matches_affine() {
        let t = t();
        let m = t.image_to_clip_matrix();
        let p = Vec2::new(321.0, 45.0);
        let clip_x = m[0][0] * p.x + m[1][0] * p.y + m[3][0];
        let clip_y = m[0][1] * p.x + m[1][1] * p.y + m[3][1];
        let s = t.image_to_screen(p);
        let ex = (s.x - t.rect.center().x) / (t.rect.width() / 2.0);
        let ey = -(s.y - t.rect.center().y) / (t.rect.height() / 2.0);
        assert!((clip_x - ex).abs() < 1e-4 && (clip_y - ey).abs() < 1e-4);
    }

    #[test]
    fn fit_zoom_fits() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 500.0));
        let z = ViewTransform::fit_zoom(rect, 2000, 2000, 1.0, 0.0);
        assert!((z - 0.245).abs() < 1e-3);
        let z90 = ViewTransform::fit_zoom(rect, 2000, 1000, 1.0, 90.0);
        assert!((z90 - 0.245).abs() < 1e-3);
    }

    #[test]
    fn length_in_mm() {
        let l = ViewTransform::length_mm(Vec2::ZERO, Vec2::new(3.0, 4.0), Some([0.5, 0.5])).unwrap();
        assert!((l - 2.5).abs() < 1e-9);
        assert!(ViewTransform::length_mm(Vec2::ZERO, Vec2::new(3.0, 4.0), None).is_none());
    }
}
