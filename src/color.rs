//! Toolkit-neutral color representation.

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn from_rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub const fn from_rgba_unmultiplied(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::from_rgba(r, g, b, a)
    }

    pub const fn from_black_alpha(alpha: u8) -> Self {
        Self::from_rgba(0, 0, 0, alpha)
    }

    pub const fn r(&self) -> u8 {
        self.r
    }

    pub const fn g(&self) -> u8 {
        self.g
    }

    pub const fn b(&self) -> u8 {
        self.b
    }

    pub const fn a(&self) -> u8 {
        self.a
    }

    pub const WHITE: Self = Self::from_rgb(255, 255, 255);

    /// Linear interpolation between two colors with gamma correction.
    /// `t = 0.0` returns `self`, `t = 1.0` returns `other`.
    pub fn lerp_to_gamma(self, other: Color, t: f32) -> Color {
        let t = t.clamp(0.0, 1.0);
        let to_linear = |c: u8| {
            let c = c as f32 / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        let to_gamma = |c: f32| {
            let c = if c <= 0.0031308 {
                c * 12.92
            } else {
                1.055 * c.powf(1.0 / 2.4) - 0.055
            };
            (c * 255.0).round().clamp(0.0, 255.0) as u8
        };

        let r = to_linear(self.r) * (1.0 - t) + to_linear(other.r) * t;
        let g = to_linear(self.g) * (1.0 - t) + to_linear(other.g) * t;
        let b = to_linear(self.b) * (1.0 - t) + to_linear(other.b) * t;
        let a = self.a as f32 * (1.0 - t) + other.a as f32 * t;

        Color::from_rgba(
            to_gamma(r),
            to_gamma(g),
            to_gamma(b),
            a.round().clamp(0.0, 255.0) as u8,
        )
    }
}
