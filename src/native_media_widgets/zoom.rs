//! The arithmetic behind the photo viewer's zoom, kept apart from any widget.
//! A scale is displayed pixels per image pixel, so 1.0 is 100%.

pub(super) const ACTUAL: f64 = 1.0;
const BUTTON_STEP: f64 = 1.25;
const WHEEL_STEP: f64 = 1.1;
const MAX_SCALE: f64 = 8.0;
const MIN_OF_FIT: f64 = 0.25;
/// GSK positions in single precision; a displayed edge past this loses pixels.
const MAX_DISPLAY_EDGE: f64 = 32_000.0;
const SNAP_TOLERANCE: f64 = 0.002;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Level {
    Fit,
    Scale(f64),
}

impl Level {
    /// Fit and 100%: what the percentage and a double click switch between.
    pub(super) fn toggled(self) -> Self {
        match self {
            Self::Fit => Self::Scale(ACTUAL),
            Self::Scale(_) => Self::Fit,
        }
    }
}

/// Scale at which the whole image just fits the view, however small the
/// image is; unknown sizes give 1.
pub(super) fn fit_scale(image: (f64, f64), view: (f64, f64)) -> f64 {
    if image.0 < 1.0 || image.1 < 1.0 || view.0 < 1.0 || view.1 < 1.0 {
        return ACTUAL;
    }
    (view.0 / image.0).min(view.1 / image.1)
}

/// The scales an image can be shown at in a view: from a quarter of fit up
/// to eight times its size, and always including fit itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Bounds {
    pub(super) fit: f64,
    pub(super) min: f64,
    pub(super) max: f64,
}

impl Bounds {
    pub(super) fn new(image: (f64, f64), view: (f64, f64)) -> Self {
        let fit = fit_scale(image, view);
        let longest = image.0.max(image.1).max(1.0);
        Self {
            fit,
            min: fit * MIN_OF_FIT,
            max: MAX_SCALE.min(MAX_DISPLAY_EDGE / longest).max(fit),
        }
    }

    pub(super) fn clamp(&self, scale: f64) -> f64 {
        scale.clamp(self.min, self.max)
    }

    pub(super) fn can_shrink(&self, scale: f64) -> bool {
        scale > self.min * (1.0 + SNAP_TOLERANCE)
    }

    pub(super) fn can_grow(&self, scale: f64) -> bool {
        scale < self.max * (1.0 - SNAP_TOLERANCE)
    }

    pub(super) fn level(&self, scale: f64) -> Level {
        Level::Scale(self.clamp(scale))
    }

    /// One step of the zoom buttons or keys, in or out. A step that would
    /// pass fit or 100% stops there instead, so both stay easy to reach.
    pub(super) fn stepped(&self, scale: f64, zoom_in: bool) -> Level {
        let factor = if zoom_in {
            BUTTON_STEP
        } else {
            1.0 / BUTTON_STEP
        };
        let target = self.clamp(scale * factor);
        let passed = |stop: f64| {
            if zoom_in {
                stop > scale * (1.0 + SNAP_TOLERANCE) && stop < target
            } else {
                stop < scale * (1.0 - SNAP_TOLERANCE) && stop > target
            }
        };
        let mut stops = [(self.fit, Level::Fit), (ACTUAL, Level::Scale(ACTUAL))];
        stops.sort_by(|a, b| (a.0 - scale).abs().total_cmp(&(b.0 - scale).abs()));
        stops
            .into_iter()
            .find(|(stop, _)| passed(*stop))
            .map_or(Level::Scale(target), |(_, level)| level)
    }

    /// The scale after a scroll of `delta` notches, positive away from the
    /// user (zooming out). Smooth scrolling gives fractions of a notch.
    pub(super) fn wheeled(&self, scale: f64, delta: f64) -> f64 {
        self.clamp(scale * WHEEL_STEP.powf(-delta))
    }
}

/// Whole percent for display, never zero.
pub(super) fn percent(scale: f64) -> u32 {
    (scale * 100.0).round().max(1.0) as u32
}

/// Length of the scrolled content along one axis: the image, or the view
/// when the image is smaller.
pub(super) fn content_len(image: f64, view: f64, scale: f64) -> f64 {
    (image * scale).max(view)
}

/// Where the image starts along one axis; small images sit in the middle.
pub(super) fn origin(image: f64, view: f64, scale: f64) -> f64 {
    (content_len(image, view, scale) - image * scale) / 2.0
}

/// The scroll offset along one axis that keeps the image point under
/// `pointer` (a position in the view) where it was when the scale changes.
pub(super) fn anchored_offset(
    image: f64,
    view: f64,
    from: f64,
    to: f64,
    offset: f64,
    pointer: f64,
) -> f64 {
    let point = (offset + pointer - origin(image, view, from)) / from;
    let target = point * to + origin(image, view, to) - pointer;
    target.clamp(0.0, (content_len(image, view, to) - view).max(0.0))
}

/// Largest side the first decode of a photo needs for a view of `view` logical
/// pixels on a screen of `factor` device pixels per logical pixel.
pub(super) fn decode_box(view: (i32, i32), factor: i32, limit: u32) -> (u32, u32) {
    let edge = |logical: i32| {
        u32::try_from(logical.max(1).saturating_mul(factor.max(1)))
            .unwrap_or(limit)
            .clamp(512.min(limit), limit)
    };
    (edge(view.0), edge(view.1))
}

/// The picture size a texture stands for: the texture's own shape, as large
/// as the sender declared the photo. A declared size smaller than the texture
/// is wrong and ignored.
pub(super) fn logical_size(texture: (u32, u32), declared: Option<(u32, u32)>) -> (f64, f64) {
    let (width, height) = (f64::from(texture.0.max(1)), f64::from(texture.1.max(1)));
    let longest = declared.map_or(0.0, |(w, h)| f64::from(w.max(h)));
    let factor = (longest / width.max(height)).max(1.0);
    (width * factor, height * factor)
}

/// How many texture pixels there are per picture pixel, at most one.
pub(super) fn density(texture: (u32, u32), logical: (f64, f64)) -> f64 {
    let longest = logical.0.max(logical.1).max(1.0);
    (f64::from(texture.0.max(texture.1)) / longest).min(1.0)
}

/// Whether a texture of `have` density looks soft at `scale` on a screen of
/// `factor` device pixels per logical pixel.
pub(super) fn needs_sharper(have: f64, scale: f64, factor: i32) -> bool {
    have < 0.999 && scale * f64::from(factor.max(1)) > have * 1.05
}

/// Density to decode next: twice what is on screen, so a run of zoom steps
/// reloads a few times rather than at every step, up to the real size.
pub(super) fn next_density(scale: f64, factor: i32) -> f64 {
    (scale * f64::from(factor.max(1)) * 2.0).min(1.0)
}

/// The size to decode a photo of `logical` at `density`, never more than
/// `max_pixels` in all.
pub(super) fn decode_size(logical: (f64, f64), density: f64, max_pixels: f64) -> (u32, u32) {
    let density = density.min(1.0);
    let pixels = logical.0 * logical.1 * density * density;
    let shrink = if pixels > max_pixels {
        (max_pixels / pixels).sqrt()
    } else {
        1.0
    };
    let side = |length: f64| {
        let side = length * density * shrink;
        (if shrink < 1.0 {
            side.floor()
        } else {
            side.round()
        })
        .max(1.0) as u32
    };
    (side(logical.0), side(logical.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const IMAGE: (f64, f64) = (4000.0, 3000.0);
    const VIEW: (f64, f64) = (1000.0, 600.0);

    fn approx(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-9, "{left} != {right}");
    }

    #[test]
    fn fit_depends_on_the_tighter_side() {
        approx(fit_scale((4000.0, 1000.0), (1000.0, 600.0)), 0.25);
        approx(fit_scale((1000.0, 4000.0), (1000.0, 600.0)), 0.15);
        approx(fit_scale((100.0, 50.0), (1000.0, 600.0)), 10.0);
        approx(fit_scale((0.0, 50.0), (1000.0, 600.0)), 1.0);
        approx(fit_scale((100.0, 50.0), (0.0, 0.0)), 1.0);
    }

    #[test]
    fn bounds_run_from_a_quarter_of_fit_to_eight_times() {
        let bounds = Bounds::new(IMAGE, VIEW);
        approx(bounds.fit, 0.2);
        approx(bounds.min, 0.05);
        approx(bounds.max, 8.0);
        approx(bounds.clamp(0.001), 0.05);
        approx(bounds.clamp(99.0), 8.0);
        approx(bounds.clamp(1.0), 1.0);
    }

    #[test]
    fn bounds_always_include_fit() {
        let tiny = Bounds::new((40.0, 30.0), (1000.0, 600.0));
        approx(tiny.fit, 20.0);
        assert!(tiny.max >= tiny.fit);
        assert!(tiny.min < tiny.fit);
    }

    #[test]
    fn huge_images_cap_the_displayed_edge() {
        let bounds = Bounds::new((40_000.0, 100.0), VIEW);
        approx(bounds.max, 0.8);
        assert!(bounds.max >= bounds.fit);
    }

    #[test]
    fn buttons_disable_at_the_limits() {
        let bounds = Bounds::new(IMAGE, VIEW);
        assert!(!bounds.can_shrink(bounds.min));
        assert!(bounds.can_shrink(bounds.fit));
        assert!(!bounds.can_grow(bounds.max));
        assert!(bounds.can_grow(1.0));
    }

    fn scale_of(level: Level) -> f64 {
        match level {
            Level::Scale(scale) => scale,
            Level::Fit => panic!("expected a scale"),
        }
    }

    #[test]
    fn steps_scale_by_a_ratio() {
        let bounds = Bounds::new(IMAGE, VIEW);
        approx(scale_of(bounds.stepped(2.0, true)), 2.5);
        approx(scale_of(bounds.stepped(2.0, false)), 1.6);
    }

    #[test]
    fn steps_stop_at_fit_and_actual_size() {
        let bounds = Bounds::new(IMAGE, VIEW);
        assert_eq!(bounds.stepped(0.17, true), Level::Fit);
        assert_eq!(bounds.stepped(0.23, false), Level::Fit);
        approx(scale_of(bounds.stepped(0.3, false)), 0.24);
        assert_eq!(bounds.stepped(0.9, true), Level::Scale(ACTUAL));
        assert_eq!(bounds.stepped(1.1, false), Level::Scale(ACTUAL));
        approx(scale_of(bounds.stepped(bounds.fit, true)), 0.25);
        approx(scale_of(bounds.stepped(ACTUAL, true)), 1.25);
        approx(scale_of(bounds.stepped(ACTUAL, false)), 0.8);
    }

    #[test]
    fn steps_stay_within_the_bounds() {
        let bounds = Bounds::new(IMAGE, VIEW);
        assert_eq!(bounds.stepped(7.9, true), Level::Scale(8.0));
        assert_eq!(bounds.stepped(0.052, false), Level::Scale(bounds.min));
        assert_eq!(bounds.stepped(8.0, true), Level::Scale(8.0));
    }

    #[test]
    fn walking_up_the_steps_reaches_the_maximum() {
        let bounds = Bounds::new(IMAGE, VIEW);
        let mut scale = bounds.min;
        for _ in 0..64 {
            scale = match bounds.stepped(scale, true) {
                Level::Fit => bounds.fit,
                Level::Scale(next) => next,
            };
        }
        approx(scale, bounds.max);
        assert!(!bounds.can_grow(scale));
    }

    #[test]
    fn the_wheel_zooms_smoothly_in_both_directions() {
        let bounds = Bounds::new(IMAGE, VIEW);
        approx(bounds.wheeled(1.0, -1.0), 1.1);
        approx(bounds.wheeled(1.1, 1.0), 1.0);
        let half = bounds.wheeled(1.0, -0.5);
        approx(half * half, 1.1);
        approx(bounds.wheeled(1.0, 0.0), 1.0);
        approx(bounds.wheeled(7.9, -5.0), 8.0);
        approx(bounds.wheeled(0.06, 5.0), bounds.min);
    }

    #[test]
    fn the_percentage_rounds_to_whole_numbers() {
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(1.2), 120);
        assert_eq!(percent(0.1249), 12);
        assert_eq!(percent(0.1251), 13);
        assert_eq!(percent(0.0001), 1);
        assert_eq!(percent(8.0), 800);
    }

    #[test]
    fn the_percentage_button_switches_between_fit_and_actual_size() {
        assert_eq!(Level::Fit.toggled(), Level::Scale(ACTUAL));
        assert_eq!(Level::Scale(ACTUAL).toggled(), Level::Fit);
        assert_eq!(Level::Scale(3.0).toggled(), Level::Fit);
    }

    fn image_point(offset: f64, pointer: f64, scale: f64, view: f64) -> f64 {
        (offset + pointer - origin(IMAGE.0, view, scale)) / scale
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let view = 1000.0;
        for (from, to, offset, pointer) in [
            (1.0, 2.0, 500.0, 300.0),
            (2.0, 1.0, 1200.0, 900.0),
            (1.0, 8.0, 0.0, 10.0),
            (0.5, 3.0, 0.0, 700.0),
            (3.0, 1.25, 4000.0, 450.0),
        ] {
            let before = image_point(offset, pointer, from, view);
            let moved = anchored_offset(IMAGE.0, view, from, to, offset, pointer);
            let after = image_point(moved, pointer, to, view);
            let clamped = moved == 0.0 || moved == content_len(IMAGE.0, view, to) - view;
            assert!(
                clamped || (before - after).abs() < 1e-6,
                "{from}->{to}: {before} vs {after}"
            );
        }
    }

    #[test]
    fn zooming_stays_inside_the_content() {
        let view = 1000.0;
        approx(anchored_offset(IMAGE.0, view, 1.0, 2.0, 0.0, 0.0), 0.0);
        let far = anchored_offset(IMAGE.0, view, 1.0, 2.0, 3000.0, 1000.0);
        approx(far, 8000.0 - view);
        approx(anchored_offset(IMAGE.0, view, 2.0, 0.1, 5000.0, 400.0), 0.0);
    }

    #[test]
    fn a_small_image_is_centred_and_zooms_about_its_middle() {
        approx(origin(400.0, 1000.0, 1.0), 300.0);
        approx(origin(400.0, 1000.0, 3.0), 0.0);
        let offset = anchored_offset(400.0, 1000.0, 1.0, 4.0, 0.0, 500.0);
        approx(offset, 300.0);
    }

    #[test]
    fn zooming_back_to_a_smaller_scale_returns_to_the_start() {
        let view = 1000.0;
        let there = anchored_offset(IMAGE.0, view, 1.0, 2.0, 1500.0, 250.0);
        let back = anchored_offset(IMAGE.0, view, 2.0, 1.0, there, 250.0);
        approx(back, 1500.0);
    }

    #[test]
    fn the_first_decode_follows_the_view_and_the_screen() {
        assert_eq!(decode_box((990, 790), 1, 3840), (990, 790));
        assert_eq!(decode_box((990, 790), 2, 3840), (1980, 1580));
        assert_eq!(decode_box((3000, 2000), 2, 3840), (3840, 3840));
        assert_eq!(decode_box((100, 100), 1, 3840), (512, 512));
        assert_eq!(decode_box((-5, 0), 0, 3840), (512, 512));
    }

    #[test]
    fn declared_sizes_scale_the_texture_never_reshape_it() {
        assert_eq!(logical_size((64, 40), Some((1600, 1000))), (1600.0, 1000.0));
        assert_eq!(logical_size((64, 40), Some((1000, 1600))), (1600.0, 1000.0));
        assert_eq!(logical_size((64, 40), None), (64.0, 40.0));
        assert_eq!(logical_size((640, 400), Some((10, 10))), (640.0, 400.0));
        assert_eq!(logical_size((0, 0), None), (1.0, 1.0));
    }

    #[test]
    fn density_counts_texture_pixels_per_picture_pixel() {
        approx(density((3000, 2000), (6000.0, 4000.0)), 0.5);
        approx(density((6000, 4000), (6000.0, 4000.0)), 1.0);
        approx(density((9000, 6000), (6000.0, 4000.0)), 1.0);
    }

    #[test]
    fn a_soft_texture_is_replaced_only_when_it_shows() {
        assert!(!needs_sharper(0.5, 0.5, 1));
        assert!(needs_sharper(0.5, 0.6, 1));
        assert!(needs_sharper(0.5, 0.3, 2));
        assert!(!needs_sharper(1.0, 8.0, 2));
        assert!(!needs_sharper(0.999, 8.0, 1));
    }

    #[test]
    fn reloads_aim_past_what_is_asked() {
        approx(next_density(0.2, 1), 0.4);
        approx(next_density(0.2, 2), 0.8);
        approx(next_density(2.0, 1), 1.0);
    }

    #[test]
    fn decoded_sizes_respect_the_pixel_budget() {
        assert_eq!(decode_size((6000.0, 4000.0), 0.5, 64e6), (3000, 2000));
        assert_eq!(decode_size((6000.0, 4000.0), 1.0, 64e6), (6000, 4000));
        let (width, height) = decode_size((12_000.0, 9000.0), 1.0, 64e6);
        assert!(u64::from(width) * u64::from(height) <= 64_000_000);
        assert!((f64::from(width) / f64::from(height) - 4.0 / 3.0).abs() < 0.001);
        assert_eq!(decode_size((10.0, 10.0), 0.0001, 64e6), (1, 1));
    }
}
