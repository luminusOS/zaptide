use super::gtk;
use crate::i18n::gettext;
use gtk::prelude::*;

pub(super) enum AttachmentRemoval {
    Clear,
    File(std::path::PathBuf),
    LinkPreview,
}

const THUMBNAIL_EDGE: i32 = 56;

pub(super) fn loading_tile(url: &str) -> gtk::Widget {
    let tile = gtk::Box::builder()
        .spacing(12)
        .height_request(super::CARD_EDGE)
        .width_request(320)
        .css_classes(["zaptide-attachment-file", "zaptide-link-tile"])
        .build();
    let placeholder = gtk::Box::builder()
        .width_request(THUMBNAIL_EDGE)
        .height_request(THUMBNAIL_EDGE)
        .valign(gtk::Align::Center)
        .css_classes(["zaptide-link-thumbnail", "zaptide-link-placeholder"])
        .build();
    placeholder.append(
        &gtk::Spinner::builder()
            .spinning(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build(),
    );
    tile.append(&placeholder);
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
        .height_request(super::CARD_EDGE)
        .width_request(320)
        .css_classes(["zaptide-attachment-file", "zaptide-link-tile"])
        .build();
    if let Some(texture) = preview
        .thumbnail
        .as_ref()
        .and_then(|bytes| gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from(bytes)).ok())
    {
        tile.append(
            &gtk::Image::builder()
                .paintable(&texture)
                .pixel_size(THUMBNAIL_EDGE)
                .valign(gtk::Align::Center)
                .overflow(gtk::Overflow::Hidden)
                .css_classes(["zaptide-link-thumbnail"])
                .build(),
        );
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
