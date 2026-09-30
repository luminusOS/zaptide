use super::*;

/// A shared place: the sender's map snapshot, name, address, and
/// coordinates. Clicking opens the default maps app, else OpenStreetMap.
pub(super) fn location_card(
    latitude: f64,
    longitude: f64,
    name: Option<&str>,
    address: Option<&str>,
    thumbnail: Option<Vec<u8>>,
    decode_token: &DecodeToken,
) -> gtk::Button {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    if let Some(bytes) = thumbnail {
        let image = gtk::Image::new();
        image.set_pixel_size(160);
        image.set_halign(gtk::Align::Start);
        image.add_css_class("zaptide-link-thumbnail");
        image.set_overflow(gtk::Overflow::Hidden);
        image.set_visible(false);
        card.append(&image);
        decode_preview_async(&image, move || Some(bytes), decode_token);
    }
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    heading.append(&gtk::Image::from_icon_name("mark-location-symbolic"));
    let title = gtk::Label::builder()
        .label(name.unwrap_or("Location"))
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(44)
        .css_classes(["heading"])
        .build();
    heading.append(&title);
    card.append(&heading);
    let coordinates = format!("{latitude:.5}, {longitude:.5}");
    for (text, classes) in [
        (address, &["caption"][..]),
        (
            Some(coordinates.as_str()),
            &["caption", "dim-label", "numeric"][..],
        ),
    ] {
        if let Some(text) = text {
            card.append(
                &gtk::Label::builder()
                    .label(text)
                    .xalign(0.0)
                    .wrap(true)
                    .max_width_chars(44)
                    .selectable(false)
                    .css_classes(classes)
                    .build(),
            );
        }
    }
    clickable_card(&card, "Open in Maps", move |button| {
        let window = button.root().and_downcast::<gtk::Window>();
        let fallback = format!(
            "https://www.openstreetmap.org/?mlat={latitude}&mlon={longitude}#map=16/{latitude}/{longitude}"
        );
        let retry = window.clone();
        gtk::UriLauncher::new(&format!("geo:{latitude},{longitude}")).launch(
            window.as_ref(),
            gtk::gio::Cancellable::NONE,
            move |result| {
                if result.is_err() {
                    gtk::UriLauncher::new(&fallback).launch(
                        retry.as_ref(),
                        gtk::gio::Cancellable::NONE,
                        |_| {},
                    );
                }
            },
        );
    })
}

/// Wraps card content in a flat button, so it takes focus and opens with
/// Enter or Space as well as a click.
fn clickable_card(
    content: &gtk::Box,
    tooltip: &str,
    on_click: impl Fn(&gtk::Button) + 'static,
) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(content)
        .tooltip_text(tooltip)
        .css_classes(["flat", "card", "zaptide-media-card", "zaptide-link-card"])
        .build();
    button.set_cursor_from_name(Some("pointer"));
    button.connect_clicked(on_click);
    button
}

/// Link preview as WhatsApp sends it: the sender's thumbnail, title,
/// description, and site. Clicking it opens the link.
pub(super) fn link_card(
    preview: &crate::model::LinkPreview,
    thumbnail: Option<Vec<u8>>,
    decode_token: &DecodeToken,
) -> gtk::Button {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    if let Some(bytes) = thumbnail {
        let image = gtk::Image::new();
        image.set_pixel_size(72);
        image.set_valign(gtk::Align::Start);
        image.add_css_class("zaptide-link-thumbnail");
        image.set_overflow(gtk::Overflow::Hidden);
        image.set_visible(false);
        card.append(&image);
        decode_preview_async(&image, move || Some(bytes), decode_token);
    }
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    let line = |value: &str, lines: i32, classes: &[&str]| {
        let label = gtk::Label::builder()
            .label(value)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(lines)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(44)
            .css_classes(classes)
            .build();
        text.append(&label);
    };
    let host = url::Url::parse(&preview.url).ok().and_then(|url| {
        url.host_str()
            .map(|host| host.trim_start_matches("www.").to_owned())
    });
    line(
        preview.title.as_deref().unwrap_or(&preview.url),
        2,
        &["heading"],
    );
    if let Some(description) = &preview.description {
        line(description, 3, &["caption"]);
    }
    if let Some(host) = &host {
        line(host, 1, &["caption", "dim-label"]);
    }
    card.append(&text);
    let url = preview.url.clone();
    clickable_card(&card, &preview.url, move |button| {
        let window = button.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(&url).launch(window.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
    })
}

/// A label for sender-written text: WhatsApp formatting and web links apply.
/// In-app chat links need the message row, so they only show as text here.
pub(super) fn add_formatted_label(parent: &gtk::Box, text: &str) -> gtk::Label {
    let label = add_label(parent, text);
    label.set_markup(&crate::safety::linkify_markup(text));
    label.connect_activate_link(|_, uri| {
        if uri.starts_with(crate::safety::CHAT_SCHEME) {
            gtk::glib::Propagation::Stop
        } else {
            gtk::glib::Propagation::Proceed
        }
    });
    label
}

pub(super) fn add_label(parent: &gtk::Box, text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(52);
    parent.append(&label);
    label
}

/// One list section for the chooser: optional heading and rows as (id, title, detail).
pub(super) type ListChoiceSection = (Option<String>, Vec<(String, String, Option<String>)>);

/// Dialog listing a message's rows; picking one calls `on_pick` with its id
/// and closes. The dialog is held weakly by its own rows.
pub(super) fn show_list_choices(
    anchor: &gtk::Button,
    title: &str,
    sections: &[ListChoiceSection],
    on_pick: impl Fn(String) + 'static,
) {
    use adw::prelude::*;
    let dialog = adw::Dialog::builder()
        .title(if title.is_empty() { "Choose" } else { title })
        .content_width(380)
        .content_height(480)
        .build();
    if !title.is_empty() {
        dialog.set_tooltip_text(Some(title));
    }
    let page = adw::PreferencesPage::new();
    let on_pick = std::rc::Rc::new(on_pick);
    let fired = std::rc::Rc::new(std::cell::Cell::new(false));
    let sections = sections.to_vec();
    let (mut section_index, mut row_index) = (0, 0);
    let mut group: Option<adw::PreferencesGroup> = None;
    let weak_dialog = dialog.downgrade();
    let pending_page = page.clone();
    gtk::glib::idle_add_local(move || {
        if weak_dialog.upgrade().is_none() {
            return gtk::glib::ControlFlow::Break;
        }
        let mut added = 0;
        while added < 12 && section_index < sections.len() {
            let (heading, rows) = &sections[section_index];
            if group.is_none() {
                let next = adw::PreferencesGroup::new();
                if let Some(heading) = heading {
                    next.set_title(&gtk::glib::markup_escape_text(heading));
                    next.set_tooltip_text(Some(heading));
                }
                pending_page.add(&next);
                group = Some(next);
            }
            if let Some((id, row_title, detail)) = rows.get(row_index) {
                // Sender text: markup off before any text is set.
                let row = adw::ActionRow::builder()
                    .use_markup(false)
                    .title_lines(2)
                    .activatable(true)
                    .build();
                row.set_title(row_title);
                if let Some(detail) = detail {
                    row.set_subtitle(detail);
                    row.set_subtitle_lines(3);
                }
                let (id, on_pick, dialog) = (id.clone(), on_pick.clone(), weak_dialog.clone());
                let fired = fired.clone();
                row.connect_activated(move |_| {
                    // The dialog closes with an animation; a second pick during
                    // it must not send a second answer.
                    if fired.replace(true) {
                        return;
                    }
                    on_pick(id.clone());
                    if let Some(dialog) = dialog.upgrade() {
                        dialog.close();
                    }
                });
                if let Some(group) = &group {
                    group.add(&row);
                }
                row_index += 1;
                added += 1;
            } else {
                group = None;
                section_index += 1;
                row_index = 0;
            }
        }
        if section_index < sections.len() {
            gtk::glib::ControlFlow::Continue
        } else {
            gtk::glib::ControlFlow::Break
        }
    });
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    dialog.present(Some(anchor));
}

/// Button with a wrapping label for an interactive message.
pub(super) fn reply_button(text: &str) -> gtk::Button {
    let label = gtk::Label::new(Some(text));
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_max_width_chars(40);
    label.set_justify(gtk::Justification::Center);
    let button = gtk::Button::new();
    button.set_child(Some(&label));
    button
}

pub(super) fn decode_preview_async(
    image: &gtk::Image,
    load: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    token: &DecodeToken,
) {
    start_preview_decode(image.downgrade(), load, token.issue());
}

/// Waits for a decode slot instead of dropping the preview, until the row is
/// recycled or destroyed.
fn start_preview_decode(
    image: glib::WeakRef<gtk::Image>,
    load: impl FnOnce() -> Option<Vec<u8>> + Send + 'static,
    ticket: crate::native_media::DecodeTicket,
) {
    if !ticket.is_current() || image.upgrade().is_none() {
        return;
    }
    let Some(permit) = ThumbnailDecodePermit::acquire() else {
        glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
            start_preview_decode(image, load, ticket);
        });
        return;
    };
    let image = glib::SendWeakRef::from(image);
    let main_context = glib::MainContext::default();
    thread::Builder::new()
        .name("zaptide-thumbnail".into())
        .spawn(move || {
            let _permit = permit;
            let decoded = load().and_then(|bytes| ticket.decode_thumbnail(&bytes));
            main_context.invoke(move || {
                let Some(image) = image.upgrade() else {
                    return;
                };
                match decoded {
                    Some(ThumbnailResult::Image(thumbnail)) => {
                        let pixels = glib::Bytes::from_owned(thumbnail.rgba);
                        let texture = gdk::MemoryTexture::new(
                            thumbnail.width as i32,
                            thumbnail.height as i32,
                            gdk::MemoryFormat::R8g8b8a8,
                            &pixels,
                            thumbnail.width as usize * 4,
                        );
                        image.set_paintable(Some(&texture));
                        image.set_visible(true);
                    }
                    Some(ThumbnailResult::Placeholder(_)) | None => {
                        image.set_tooltip_text(Some("Image preview unavailable; open attachment"));
                        image.set_visible(false);
                    }
                }
            });
        })
        .ok();
}
