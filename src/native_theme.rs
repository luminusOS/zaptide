//! GTK styling adapter for the toolkit-neutral ZapTide palette.

use crate::theme::Palette;

fn css_color(color: crate::color::Color) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r(), color.g(), color.b())
}

fn linear_channel(channel: u8) -> f32 {
    let channel = f32::from(channel) / 255.0;
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

fn luminance(color: crate::color::Color) -> f32 {
    0.2126 * linear_channel(color.r())
        + 0.7152 * linear_channel(color.g())
        + 0.0722 * linear_channel(color.b())
}

fn contrast_ratio(first: crate::color::Color, second: crate::color::Color) -> f32 {
    let first = luminance(first);
    let second = luminance(second);
    (first.max(second) + 0.05) / (first.min(second) + 0.05)
}

fn readable_foreground(
    background: crate::color::Color,
    preferred: crate::color::Color,
) -> crate::color::Color {
    if contrast_ratio(background, preferred) >= 4.5 {
        return preferred;
    }
    let black = crate::color::Color::from_rgb(0, 0, 0);
    let white = crate::color::Color::WHITE;
    if contrast_ratio(background, black) >= contrast_ratio(background, white) {
        black
    } else {
        white
    }
}

/// Build scoped libadwaita color overrides from an existing ZapTide palette.
///
/// This leaves spacing, radii, focus rings, and widget state rendering to GTK,
/// replacing only palette colors.
pub fn css_for_palette(palette: &Palette) -> String {
    let window = css_color(palette.window);
    let panel = css_color(palette.panel);
    let surface = css_color(palette.surface);
    let hover = css_color(palette.surface_hover);
    let outline = css_color(palette.outline);
    let window_text = css_color(readable_foreground(palette.window, palette.text));
    let view_text = css_color(readable_foreground(palette.chat, palette.text));
    let panel_text = css_color(readable_foreground(palette.panel, palette.text));
    let surface_text = css_color(readable_foreground(palette.surface, palette.text));
    let secondary = css_color(readable_foreground(palette.surface, palette.secondary));
    let accent = css_color(palette.accent);
    let on_accent = css_color(readable_foreground(palette.accent, palette.on_accent));
    let danger = css_color(palette.danger);
    let danger_text = css_color(readable_foreground(palette.danger, palette.text));
    let chat = css_color(palette.chat);
    let bubble_in = css_color(palette.bubble_in);
    let bubble_out = css_color(palette.bubble_out);
    let bubble_in_text = css_color(readable_foreground(palette.bubble_in, palette.text));
    let bubble_out_text = css_color(readable_foreground(palette.bubble_out, palette.text));

    format!(
        "@define-color window_bg_color {window};\n\
         @define-color window_fg_color {window_text};\n\
         @define-color view_bg_color {chat};\n\
         @define-color view_fg_color {view_text};\n\
         @define-color headerbar_bg_color {panel};\n\
         @define-color headerbar_fg_color {panel_text};\n\
         @define-color sidebar_bg_color {panel};\n\
         @define-color sidebar_fg_color {panel_text};\n\
         @define-color card_bg_color {surface};\n\
         @define-color card_fg_color {surface_text};\n\
         @define-color popover_bg_color {surface};\n\
         @define-color popover_fg_color {surface_text};\n\
         @define-color accent_bg_color {accent};\n\
         @define-color accent_fg_color {on_accent};\n\
         @define-color accent_color {accent};\n\
         @define-color destructive_bg_color {danger};\n\
         @define-color destructive_fg_color {danger_text};\n\
         @define-color borders {outline};\n\
         @define-color secondary_text_color {secondary};\n\
         @define-color zaptide_bubble_in {bubble_in};\n\
         @define-color zaptide_bubble_out {bubble_out};\n\
         @define-color zaptide_bubble_in_text {bubble_in_text};\n\
         @define-color zaptide_bubble_out_text {bubble_out_text};\n\
         window, .background {{ background-color: @window_bg_color; color: @window_fg_color; }}\n\
         headerbar, .titlebar, .sidebar {{ background-color: @headerbar_bg_color; color: @headerbar_fg_color; }}\n\
         .view, list, listview, textview {{ background-color: @view_bg_color; color: @view_fg_color; }}\n\
         card, .card, popover {{ background-color: @card_bg_color; color: @card_fg_color; }}\n\
         entry, textview {{ caret-color: @accent_color; }}\n\
         button {{ border-color: @borders; }}\n\
         button:hover {{ background-color: {hover}; }}\n\
         button.suggested-action {{ background-color: @accent_bg_color; color: @accent_fg_color; }}\n\
         button.destructive-action {{ background-color: @destructive_bg_color; color: @destructive_fg_color; }}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::{contrast_ratio, css_color, css_for_palette, readable_foreground};
    use crate::theme::Palette;

    #[test]
    fn custom_palette_maps_colors_without_replacing_libadwaita_layout() {
        let mut palette = Palette::dark();
        palette.window = crate::color::Color::from_rgb(0x12, 0x34, 0x56);
        palette.accent = crate::color::Color::from_rgb(0xab, 0xcd, 0xef);

        let css = css_for_palette(&palette);

        assert!(css.contains("@define-color window_bg_color #123456"));
        assert!(css.contains("@define-color accent_bg_color #abcdef"));
        assert!(css.contains(&format!(
            "@define-color zaptide_bubble_in {};",
            css_color(palette.bubble_in)
        )));
        assert!(css.contains(&format!(
            "@define-color zaptide_bubble_out {};",
            css_color(palette.bubble_out)
        )));
        assert!(css.contains("button.suggested-action"));
        assert!(!css.contains("border-radius"));
        assert!(!css.contains("padding"));
    }

    #[test]
    fn destructive_action_text_keeps_readable_contrast_in_dark_palette() {
        let palette = Palette::dark();
        let foreground = readable_foreground(palette.danger, palette.text);
        assert!(contrast_ratio(palette.danger, foreground) >= 4.5);
        assert!(css_for_palette(&palette).contains(&format!(
            "@define-color destructive_fg_color {};",
            css_color(foreground)
        )));
    }

    #[test]
    fn bubble_foregrounds_keep_readable_contrast_with_custom_palette() {
        let mut palette = Palette::dark();
        palette.bubble_in = crate::color::Color::from_rgb(0x15, 0x25, 0x35);
        palette.bubble_out = crate::color::Color::from_rgb(0xdd, 0xee, 0xbb);
        let css = css_for_palette(&palette);
        for (name, background) in [("in", palette.bubble_in), ("out", palette.bubble_out)] {
            let foreground = readable_foreground(background, palette.text);
            assert!(contrast_ratio(background, foreground) >= 4.5);
            assert!(css.contains(&format!(
                "@define-color zaptide_bubble_{name}_text {};",
                css_color(foreground)
            )));
        }
    }

    #[test]
    fn low_contrast_light_palette_gets_readable_text_and_action_foregrounds() {
        let mut palette = Palette::dark();
        palette.window = crate::color::Color::WHITE;
        palette.chat = crate::color::Color::from_rgb(250, 250, 250);
        palette.panel = crate::color::Color::from_rgb(245, 245, 245);
        palette.surface = crate::color::Color::from_rgb(240, 240, 240);
        palette.text = crate::color::Color::from_rgb(230, 230, 230);
        palette.secondary = crate::color::Color::from_rgb(220, 220, 220);
        palette.accent = crate::color::Color::from_rgb(245, 245, 245);
        palette.on_accent = crate::color::Color::from_rgb(230, 230, 230);
        palette.danger = crate::color::Color::from_rgb(235, 235, 235);

        let css = css_for_palette(&palette);

        for (background, foreground) in [
            (palette.window, palette.text),
            (palette.chat, palette.text),
            (palette.panel, palette.text),
            (palette.surface, palette.text),
            (palette.surface, palette.secondary),
            (palette.accent, palette.on_accent),
            (palette.danger, palette.text),
        ] {
            assert!(contrast_ratio(background, readable_foreground(background, foreground)) >= 4.5);
        }
        assert!(css.contains("@define-color window_fg_color #000000"));
        assert!(css.contains("@define-color secondary_text_color #000000"));
        assert!(css.contains("@define-color accent_fg_color #000000"));
        assert!(css.contains("@define-color destructive_fg_color #000000"));
    }

    #[test]
    fn low_contrast_dark_palette_gets_readable_text_and_action_foregrounds() {
        let mut palette = Palette::dark();
        palette.window = crate::color::Color::from_rgb(0, 0, 0);
        palette.chat = crate::color::Color::from_rgb(5, 5, 5);
        palette.panel = crate::color::Color::from_rgb(10, 10, 10);
        palette.surface = crate::color::Color::from_rgb(15, 15, 15);
        palette.text = crate::color::Color::from_rgb(20, 20, 20);
        palette.secondary = crate::color::Color::from_rgb(25, 25, 25);
        palette.accent = crate::color::Color::from_rgb(10, 10, 10);
        palette.on_accent = crate::color::Color::from_rgb(20, 20, 20);
        palette.danger = crate::color::Color::from_rgb(15, 15, 15);

        let css = css_for_palette(&palette);

        for (background, foreground) in [
            (palette.window, palette.text),
            (palette.chat, palette.text),
            (palette.panel, palette.text),
            (palette.surface, palette.text),
            (palette.surface, palette.secondary),
            (palette.accent, palette.on_accent),
            (palette.danger, palette.text),
        ] {
            assert!(contrast_ratio(background, readable_foreground(background, foreground)) >= 4.5);
        }
        assert!(css.contains("@define-color window_fg_color #ffffff"));
        assert!(css.contains("@define-color secondary_text_color #ffffff"));
        assert!(css.contains("@define-color accent_fg_color #ffffff"));
        assert!(css.contains("@define-color destructive_fg_color #ffffff"));
    }
}
