use super::gtk;
use crate::i18n::gettext;
use gtk::prelude::*;

pub(super) enum AttachmentRemoval {
    Clear,
    File(std::path::PathBuf),
    LinkPreview,
}

const PREVIEW_TILE_EDGE: i32 = 88;
const THUMBNAIL_EDGE: i32 = 72;

pub(super) fn loading_tile(url: &str) -> gtk::Widget {
    let tile = gtk::Box::builder()
        .spacing(12)
        .height_request(PREVIEW_TILE_EDGE)
        .width_request(320)
        .css_classes(["zaptide-attachment-file", "zaptide-link-tile"])
        .build();
    let spinner = gtk::Spinner::builder()
        .spinning(true)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    tile.append(&thumbnail_slot(
        spinner.upcast(),
        &["zaptide-link-placeholder"],
    ));
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(gettext("Loading link preview"))
            .xalign(0.0)
            .css_classes(["heading", "dim-label"])
            .build(),
    );
    text.append(
        &gtk::Label::builder()
            .label(host_of(url).as_deref().unwrap_or(url))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(44)
            .css_classes(["caption", "dim-label"])
            .build(),
    );
    tile.append(&text);
    tile.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Loading link preview",
    ))]);
    tile.upcast()
}

fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok().and_then(|url| {
        url.host_str()
            .map(|host| host.trim_start_matches("www.").to_owned())
    })
}

pub(super) fn link_preview_tile(preview: &crate::model::LinkPreview) -> gtk::Widget {
    let tile = gtk::Box::builder()
        .spacing(12)
        .height_request(PREVIEW_TILE_EDGE)
        .width_request(320)
        .css_classes(["zaptide-attachment-file", "zaptide-link-tile"])
        .build();
    if let Some(texture) = preview
        .thumbnail
        .as_ref()
        .and_then(|bytes| gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from(bytes)).ok())
    {
        let image = gtk::Picture::for_paintable(&texture);
        image.set_content_fit(gtk::ContentFit::Cover);
        image.set_can_shrink(true);
        image.set_size_request(THUMBNAIL_EDGE, THUMBNAIL_EDGE);
        tile.append(&thumbnail_slot(image.upcast(), &[]));
    }
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    let line = |value: &str, lines: i32, classes: &[&str]| {
        gtk::Label::builder()
            .label(value)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(lines)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(44)
            .css_classes(classes)
            .build()
    };
    text.append(&line(
        preview.title.as_deref().unwrap_or(&preview.url),
        1,
        &["heading"],
    ));
    if let Some(description) = &preview.description {
        text.append(&line(description, 2, &["caption"]));
    }
    let host = host_of(&preview.url);
    text.append(&line(
        host.as_deref().unwrap_or(&preview.url),
        1,
        &["caption", "dim-label"],
    ));
    tile.append(&text);
    tile.upcast()
}

fn thumbnail_slot(child: gtk::Widget, classes: &[&str]) -> gtk::Widget {
    let bounds = gtk::DrawingArea::builder()
        .content_width(THUMBNAIL_EDGE)
        .content_height(THUMBNAIL_EDGE)
        .build();
    let slot = gtk::Overlay::builder()
        .child(&bounds)
        .hexpand(false)
        .vexpand(false)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["zaptide-link-thumbnail"])
        .build();
    for class in classes {
        slot.add_css_class(class);
    }
    slot.add_overlay(&child);
    slot.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a display"]
    fn preview_thumbnail_stays_bounded_and_loading_spinner_is_centered() {
        gtk::init().unwrap();
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(192, 192)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let preview = crate::model::LinkPreview {
            url: "https://example.com".into(),
            title: Some("Example".into()),
            description: Some("A short description".into()),
            thumbnail: Some(png.into_inner()),
        };
        for tile in [loading_tile(&preview.url), link_preview_tile(&preview)] {
            let (_, height, _, _) = tile.measure(gtk::Orientation::Vertical, 640);
            tile.allocate(640, height, -1, None);
            let slot = tile.first_child().unwrap();
            assert_eq!(
                (slot.width(), slot.height()),
                (THUMBNAIL_EDGE, THUMBNAIL_EDGE)
            );
            let overlay = slot.downcast::<gtk::Overlay>().unwrap();
            let content = overlay.child().unwrap().next_sibling().unwrap();
            let bounds = content.compute_bounds(&overlay).unwrap();
            let center = THUMBNAIL_EDGE as f32 / 2.0;
            assert!((bounds.x() + bounds.width() / 2.0 - center).abs() < 1.0);
            assert!((bounds.y() + bounds.height() / 2.0 - center).abs() < 1.0);
        }
    }
}
