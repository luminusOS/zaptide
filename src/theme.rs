//! Conversation and interface colors.
//!
//! [`Palette`] holds light and dark theme colors, including message bubbles.

use crate::color::Color;

pub mod custom;
#[cfg(target_os = "linux")]
mod omarchy;
pub(crate) mod presets;
#[cfg(target_os = "linux")]
mod watch;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    pub window: Color,
    pub panel: Color,
    pub surface: Color,
    pub surface_hover: Color,
    pub surface_active: Color,
    pub outline: Color,
    pub text: Color,
    pub secondary: Color,
    pub dim: Color,
    pub accent: Color,
    pub accent_hover: Color,
    pub on_accent: Color,
    pub danger: Color,
    pub warning: Color,
    pub overlay: Color,
    pub shadow: Color,
    /// Conversation background behind message bubbles.
    pub chat: Color,
    /// Incoming message bubble.
    pub bubble_in: Color,
    /// Outgoing message bubble.
    pub bubble_out: Color,
    pub link: Color,
    /// Read-receipt blue.
    pub read: Color,
}

impl serde::Serialize for Palette {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("Palette", 23)?;
        let to_hex = |c: Color| format!("#{:02x}{:02x}{:02x}{:02x}", c.r(), c.g(), c.b(), c.a());
        state.serialize_field("dark", &self.dark)?;
        state.serialize_field("window", &to_hex(self.window))?;
        state.serialize_field("panel", &to_hex(self.panel))?;
        state.serialize_field("surface", &to_hex(self.surface))?;
        state.serialize_field("surface_hover", &to_hex(self.surface_hover))?;
        state.serialize_field("surface_active", &to_hex(self.surface_active))?;
        state.serialize_field("outline", &to_hex(self.outline))?;
        state.serialize_field("text", &to_hex(self.text))?;
        state.serialize_field("secondary", &to_hex(self.secondary))?;
        state.serialize_field("dim", &to_hex(self.dim))?;
        state.serialize_field("accent", &to_hex(self.accent))?;
        state.serialize_field("accent_hover", &to_hex(self.accent_hover))?;
        state.serialize_field("on_accent", &to_hex(self.on_accent))?;
        state.serialize_field("danger", &to_hex(self.danger))?;
        state.serialize_field("warning", &to_hex(self.warning))?;
        state.serialize_field("overlay", &to_hex(self.overlay))?;
        state.serialize_field("shadow", &to_hex(self.shadow))?;
        state.serialize_field("chat", &to_hex(self.chat))?;
        state.serialize_field("bubble_in", &to_hex(self.bubble_in))?;
        state.serialize_field("bubble_out", &to_hex(self.bubble_out))?;
        state.serialize_field("link", &to_hex(self.link))?;
        state.serialize_field("read", &to_hex(self.read))?;
        state.end()
    }
}

impl<'de> serde::Deserialize<'de> for Palette {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct PaletteHelper {
            dark: bool,
            window: String,
            panel: String,
            surface: String,
            surface_hover: String,
            surface_active: String,
            outline: String,
            text: String,
            secondary: String,
            dim: String,
            accent: String,
            accent_hover: String,
            on_accent: String,
            danger: String,
            warning: String,
            overlay: String,
            shadow: String,
            chat: String,
            bubble_in: String,
            bubble_out: String,
            link: String,
            read: String,
        }
        let helper = PaletteHelper::deserialize(deserializer)?;
        let parse = |s: &str| -> Result<Color, D::Error> {
            let s = s.trim_start_matches('#');
            if s.len() != 8 {
                return Err(serde::de::Error::custom("invalid color length"));
            }
            let r = u8::from_str_radix(&s[0..2], 16).map_err(serde::de::Error::custom)?;
            let g = u8::from_str_radix(&s[2..4], 16).map_err(serde::de::Error::custom)?;
            let b = u8::from_str_radix(&s[4..6], 16).map_err(serde::de::Error::custom)?;
            let a = u8::from_str_radix(&s[6..8], 16).map_err(serde::de::Error::custom)?;
            Ok(Color::from_rgba_unmultiplied(r, g, b, a))
        };
        Ok(Palette {
            dark: helper.dark,
            window: parse(&helper.window)?,
            panel: parse(&helper.panel)?,
            surface: parse(&helper.surface)?,
            surface_hover: parse(&helper.surface_hover)?,
            surface_active: parse(&helper.surface_active)?,
            outline: parse(&helper.outline)?,
            text: parse(&helper.text)?,
            secondary: parse(&helper.secondary)?,
            dim: parse(&helper.dim)?,
            accent: parse(&helper.accent)?,
            accent_hover: parse(&helper.accent_hover)?,
            on_accent: parse(&helper.on_accent)?,
            danger: parse(&helper.danger)?,
            warning: parse(&helper.warning)?,
            overlay: parse(&helper.overlay)?,
            shadow: parse(&helper.shadow)?,
            chat: parse(&helper.chat)?,
            bubble_in: parse(&helper.bubble_in)?,
            bubble_out: parse(&helper.bubble_out)?,
            link: parse(&helper.link)?,
            read: parse(&helper.read)?,
        })
    }
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            window: Color::from_rgb(11, 20, 26),
            panel: Color::from_rgb(17, 27, 33),
            surface: Color::from_rgb(32, 44, 51),
            surface_hover: Color::from_rgb(42, 57, 66),
            surface_active: Color::from_rgb(53, 68, 77),
            outline: Color::from_rgb(34, 45, 52),
            text: Color::from_rgb(233, 237, 239),
            secondary: Color::from_rgb(134, 150, 160),
            dim: Color::from_rgb(102, 119, 129),
            accent: Color::from_rgb(0, 168, 132),
            accent_hover: Color::from_rgb(6, 207, 156),
            on_accent: Color::from_rgb(11, 20, 26),
            danger: Color::from_rgb(241, 92, 109),
            warning: Color::from_rgb(255, 210, 121),
            overlay: Color::from_rgb(35, 49, 56),
            shadow: Color::from_black_alpha(140),
            chat: Color::from_rgb(11, 20, 26),
            bubble_in: Color::from_rgb(32, 44, 51),
            bubble_out: Color::from_rgb(0, 92, 75),
            link: Color::from_rgb(83, 189, 235),
            read: Color::from_rgb(83, 189, 235),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            window: Color::from_rgb(240, 242, 245),
            panel: Color::from_rgb(255, 255, 255),
            surface: Color::from_rgb(240, 242, 245),
            surface_hover: Color::from_rgb(230, 233, 236),
            surface_active: Color::from_rgb(217, 221, 225),
            outline: Color::from_rgb(233, 237, 239),
            text: Color::from_rgb(17, 27, 33),
            secondary: Color::from_rgb(102, 119, 129),
            dim: Color::from_rgb(143, 156, 165),
            accent: Color::from_rgb(0, 168, 132),
            accent_hover: Color::from_rgb(0, 143, 111),
            on_accent: Color::WHITE,
            danger: Color::from_rgb(234, 0, 56),
            warning: Color::from_rgb(160, 107, 0),
            overlay: Color::from_rgb(255, 255, 255),
            shadow: Color::from_black_alpha(50),
            chat: Color::from_rgb(239, 234, 226),
            bubble_in: Color::from_rgb(255, 255, 255),
            bubble_out: Color::from_rgb(217, 253, 211),
            link: Color::from_rgb(2, 126, 181),
            read: Color::from_rgb(83, 189, 235),
        }
    }
}
