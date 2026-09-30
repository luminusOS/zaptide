//! Standalone dialogs owned by the root UI component.

use super::*;

pub(super) type DialogActionCallback = std::rc::Rc<dyn Fn(DialogAction)>;

pub(super) enum DialogAction {
    SendText(String),
    ClearAttachments,
    ArchiveChat(String),
    UnlinkConfirmed,
    CreatePoll(crate::model::PollDraft),
    StartChat { id: String, name: String },
    NewContact { phone: String, name: Option<String> },
    InsertMentionId(String),
    ForwardSelected(String),
    SendSticker(std::path::PathBuf),
}

/// Colored round icon over a caption for one tile of the attach grid.
pub(super) fn attach_tile(icon: &str, label: &str, tone: &str) -> gtk::Box {
    let tile = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let image = gtk::Image::builder()
        .icon_name(icon)
        .pixel_size(20)
        .halign(gtk::Align::Center)
        .css_classes(["zaptide-attach-icon", tone])
        .build();
    tile.append(&image);
    tile.append(
        &gtk::Label::builder()
            .label(label)
            .css_classes(["caption"])
            .build(),
    );
    tile
}

/// "Send Image", "Send 3 Files": images when every item is one.
fn attachment_preview_title(count: usize, images: bool) -> String {
    let noun = if images { "Image" } else { "File" };
    if count == 1 {
        format!("Send {noun}")
    } else {
        format!("Send {count} {noun}s")
    }
}

fn is_image_file(path: &std::path::Path) -> bool {
    gtk::gio::content_type_guess(Some(path), None)
        .0
        .starts_with("image/")
}

pub(super) fn show_attachment_preview_dialog(
    parent: &adw::ApplicationWindow,
    on_action: &DialogActionCallback,
    image: Option<gtk::gdk::Texture>,
    paths: &[std::path::PathBuf],
    draft: &str,
) {
    let carousel = adw::Carousel::builder().vexpand(true).spacing(12).build();
    let picture = |picture: gtk::Picture| {
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_can_shrink(true);
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture
    };
    if let Some(texture) = &image {
        carousel.append(&picture(gtk::Picture::for_paintable(texture)));
    }
    for path in paths {
        // ponytail: decodes on the main thread; fine for a few photos, move
        // to glycin like the timeline if large batches stall the window.
        if is_image_file(path) {
            carousel.append(&picture(gtk::Picture::for_filename(path)));
            continue;
        }
        let page = adw::StatusPage::builder()
            .icon_name("text-x-generic-symbolic")
            .title(
                path.file_name()
                    .map(|name| name.to_string_lossy())
                    .unwrap_or_default(),
            )
            .hexpand(true)
            .build();
        page.add_css_class("compact");
        carousel.append(&page);
    }
    let count = carousel.n_pages() as usize;
    let dots = adw::CarouselIndicatorDots::builder()
        .carousel(&carousel)
        .visible(count > 1)
        .build();

    let caption = gtk::Entry::builder()
        .placeholder_text("Add a caption")
        .text(draft)
        .hexpand(true)
        .build();
    let send = gtk::Button::builder()
        .child(&paper_plane_icon())
        .tooltip_text("Send")
        .valign(gtk::Align::Center)
        .css_classes(["circular", "suggested-action"])
        .build();
    send.update_property(&[gtk::accessible::Property::Label("Send")]);
    let bar = gtk::Box::builder()
        .spacing(6)
        .margin_top(6)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    bar.append(&caption);
    bar.append(&send);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&carousel);
    content.append(&dots);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&content));
    view.add_bottom_bar(&bar);
    let dialog = adw::Dialog::builder()
        .title(attachment_preview_title(
            count,
            image.is_some() || paths.iter().all(|path| is_image_file(path)),
        ))
        .content_width(520)
        .content_height(560)
        .child(&view)
        .build();

    let sent = std::rc::Rc::new(std::cell::Cell::new(false));
    let (close, send_action, sending) = (dialog.clone(), on_action.clone(), sent.clone());
    let entry = caption.clone();
    send.connect_clicked(move |_| {
        sending.set(true);
        send_action(DialogAction::SendText(entry.text().to_string()));
        close.close();
    });
    let button = send.clone();
    caption.connect_activate(move |_| button.emit_clicked());
    let on_action = on_action.clone();
    dialog.connect_closed(move |_| {
        if !sent.get() {
            on_action(DialogAction::ClearAttachments);
        }
    });
    // Keep the draft that became the caption instead of selecting it, so
    // typing adds to it.
    caption.connect_has_focus_notify(|entry| {
        let entry = entry.clone();
        gtk::glib::idle_add_local_once(move || entry.set_position(-1));
    });
    dialog.set_focus(Some(&caption));
    dialog.present(Some(parent));
}

pub(super) fn show_archive_confirmation(
    parent: &adw::ApplicationWindow,
    on_action: &DialogActionCallback,
    chat: &crate::model::Chat,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Archive Chat?")
        .body(format!(
            "{} moves to your archived chats. You can unarchive it at any time.",
            chat.name
        ))
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("archive", "Archive");
    dialog.set_response_appearance("archive", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("archive"));
    dialog.set_close_response("cancel");
    let (on_action, id) = (on_action.clone(), chat.id.clone());
    dialog.connect_response(None, move |_, response| {
        if response == "archive" {
            on_action(DialogAction::ArchiveChat(id.clone()));
        }
    });
    dialog.present(Some(parent));
}

pub(super) fn show_unlink_confirmation(
    parent: &adw::ApplicationWindow,
    on_action: &DialogActionCallback,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Unlink this computer?")
        .body(
            "This removes this device from your linked devices and clears its local conversations.",
        )
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("unlink", "Unlink");
    dialog.set_response_appearance("unlink", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let on_action = on_action.clone();
    dialog.connect_response(None, move |_, response| {
        if response == "unlink" {
            on_action(DialogAction::UnlinkConfirmed);
        }
    });
    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::attachment_preview_title;

    #[test]
    fn attachment_preview_titles_count_images_and_files() {
        assert_eq!(attachment_preview_title(1, true), "Send Image");
        assert_eq!(attachment_preview_title(3, true), "Send 3 Images");
        assert_eq!(attachment_preview_title(2, false), "Send 2 Files");
    }
}

/// New Poll: a question, 2–12 answers added or removed in place, and a
/// multiple-answer switch. Create stays disabled until the draft is valid.
pub(super) fn show_poll_dialog(parent: &adw::ApplicationWindow, on_action: &DialogActionCallback) {
    use std::{cell::RefCell, rc::Rc};
    const MAX_OPTIONS: usize = 12;
    let dialog = adw::Dialog::builder()
        .title("New Poll")
        .content_width(420)
        .content_height(560)
        .build();
    let question = adw::EntryRow::builder()
        .title("Question")
        .activates_default(true)
        .build();
    let options_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let add = adw::ButtonRow::builder()
        .title("Add Option")
        .start_icon_name("list-add-symbolic")
        .build();
    options_list.append(&add);
    let multiple = adw::SwitchRow::builder()
        .title("Allow Multiple Answers")
        .build();
    let create = gtk::Button::builder()
        .label("Create")
        .css_classes(["suggested-action"])
        .sensitive(false)
        .build();
    let options: Rc<RefCell<Vec<(adw::EntryRow, gtk::Button)>>> = Rc::default();

    // Closures below hold widgets weakly and `options` is emptied when the
    // dialog closes, so no reference cycle keeps the dialog alive.
    let draft = {
        let (question, options, multiple) =
            (question.downgrade(), options.clone(), multiple.downgrade());
        move || crate::model::PollDraft {
            question: question
                .upgrade()
                .map_or_else(String::new, |question| question.text().into()),
            options: options
                .borrow()
                .iter()
                .map(|(row, _)| row.text().into())
                .collect(),
            multiple: multiple
                .upgrade()
                .is_some_and(|multiple| multiple.is_active()),
        }
    };
    let refresh: Rc<dyn Fn()> = {
        let (draft, options, create, add) = (
            draft.clone(),
            options.clone(),
            create.downgrade(),
            add.downgrade(),
        );
        Rc::new(move || {
            let (Some(create), Some(add)) = (create.upgrade(), add.upgrade()) else {
                return;
            };
            let rows = options.borrow();
            for (index, (row, remove)) in rows.iter().enumerate() {
                row.set_title(&format!("Option {}", index + 1));
                remove.set_visible(rows.len() > 2);
            }
            add.set_visible(rows.len() < MAX_OPTIONS);
            let valid = draft().validated();
            create.set_sensitive(valid.is_ok());
            create.set_tooltip_text(valid.err());
        })
    };
    let add_option: Rc<dyn Fn()> = {
        let (list, options, refresh) = (options_list.downgrade(), options.clone(), refresh.clone());
        Rc::new(move || {
            let Some(list) = list.upgrade() else {
                return;
            };
            let row = adw::EntryRow::builder().activates_default(true).build();
            let remove = gtk::Button::builder()
                .icon_name("list-remove-symbolic")
                .tooltip_text("Remove option")
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            row.add_suffix(&remove);
            let refresh_on_edit = refresh.clone();
            row.connect_changed(move |_| refresh_on_edit());
            let (list_ref, options_ref, refresh_ref, target) = (
                list.downgrade(),
                options.clone(),
                refresh.clone(),
                row.downgrade(),
            );
            remove.connect_clicked(move |_| {
                let (Some(list), Some(target)) = (list_ref.upgrade(), target.upgrade()) else {
                    return;
                };
                options_ref.borrow_mut().retain(|(row, _)| row != &target);
                list.remove(&target);
                refresh_ref();
            });
            let position = options.borrow().len() as i32;
            list.insert(&row, position);
            options.borrow_mut().push((row.clone(), remove));
            refresh();
            row.grab_focus();
        })
    };
    add_option();
    add_option();
    let add_clicked = add_option.clone();
    add.connect_activated(move |_| add_clicked());
    let refresh_question = refresh.clone();
    question.connect_changed(move |_| refresh_question());
    let refresh_multiple = refresh.clone();
    multiple.connect_active_notify(move |_| refresh_multiple());

    let page = adw::PreferencesPage::new();
    let group = adw::PreferencesGroup::new();
    group.add(&question);
    page.add(&group);
    let group = adw::PreferencesGroup::builder().title("Options").build();
    group.add(&options_list);
    page.add(&group);
    let group = adw::PreferencesGroup::new();
    group.add(&multiple);
    page.add(&group);

    let cancel = gtk::Button::with_label("Cancel");
    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(close) = close.upgrade() {
            close.close();
        }
    });
    let (close, on_action) = (dialog.downgrade(), on_action.clone());
    create.connect_clicked(move |_| {
        if let Ok(draft) = draft().validated() {
            on_action(DialogAction::CreatePoll(draft));
            if let Some(close) = close.upgrade() {
                close.close();
            }
        }
    });
    dialog.connect_closed(move |_| options.borrow_mut().clear());
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&create);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&page));
    dialog.set_child(Some(&view));
    dialog.set_default_widget(Some(&create));
    dialog.set_focus(Some(&question));
    dialog.present(Some(parent));
}

/// Picks a contact to message, or adds a number through New Contact.
pub(super) fn show_new_chat_dialog(
    parent: &adw::ApplicationWindow,
    on_action: &DialogActionCallback,
    contacts: Vec<(String, String, String)>,
) {
    let dialog = adw::Dialog::builder()
        .title("New Chat")
        .content_width(400)
        .content_height(560)
        .build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search contacts")
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(6)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(18)
        .margin_top(6)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let actions = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let add = adw::ActionRow::builder()
        .title("New Contact")
        .subtitle("Message a phone number")
        .activatable(true)
        .build();
    add.add_prefix(&gtk::Image::from_icon_name("contact-new-symbolic"));
    add.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let (close, window, contact_action) = (dialog.clone(), parent.clone(), on_action.clone());
    add.connect_activated(move |_| {
        close.close();
        show_new_contact_dialog(&window, &contact_action);
    });
    actions.append(&add);
    content.append(&actions);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let placeholder = gtk::Label::builder()
        .label(if contacts.is_empty() {
            "Contacts from your phone appear here once they sync."
        } else {
            "No contacts match this search."
        })
        .wrap(true)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["dim-label"])
        .build();
    list.set_placeholder(Some(&placeholder));
    let rows = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    for (id, name, phone) in contacts {
        let row = adw::ActionRow::builder()
            .title(&name)
            .use_markup(false)
            .title_lines(1)
            .activatable(true)
            .build();
        if name != phone {
            row.set_subtitle(&phone);
        }
        row.add_prefix(&adw::Avatar::new(32, Some(&name), true));
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        rows.borrow_mut()
            .push((row.clone(), format!("{} {digits}", name.to_lowercase())));
        let (close, on_action) = (dialog.clone(), on_action.clone());
        row.connect_activated(move |_| {
            on_action(DialogAction::StartChat {
                id: id.clone(),
                name: name.clone(),
            });
            close.close();
        });
        list.append(&row);
    }
    search.connect_search_changed(move |entry| {
        let needle = entry.text().trim().to_lowercase();
        for (row, key) in rows.borrow().iter() {
            row.set_visible(needle.is_empty() || key.contains(&needle));
        }
    });
    content.append(&list);

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.add_top_bar(&search);
    view.set_content(Some(&scroll));
    dialog.set_child(Some(&view));
    dialog.set_focus(Some(&search));
    dialog.present(Some(parent));
}

pub(super) fn show_new_contact_dialog(
    parent: &adw::ApplicationWindow,
    on_action: &DialogActionCallback,
) {
    let dialog = gtk::Window::builder()
        .title("New contact")
        .transient_for(parent)
        .modal(true)
        .default_width(380)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(18)
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let phone = gtk::Entry::builder()
        .placeholder_text("Phone number, including country code")
        .input_purpose(gtk::InputPurpose::Phone)
        .activates_default(true)
        .build();
    let name = gtk::Entry::builder()
        .placeholder_text("Name (optional)")
        .activates_default(true)
        .build();
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let add = gtk::Button::with_label("Add contact");
    add.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&add);
    content.append(&phone);
    content.append(&name);
    content.append(&buttons);
    dialog.set_child(Some(&content));
    dialog.set_default_widget(Some(&add));
    let close = dialog.clone();
    cancel.connect_clicked(move |_| close.close());
    let close = dialog.clone();
    let on_action = on_action.clone();
    let phone_input = phone.clone();
    add.connect_clicked(move |_| {
        on_action(DialogAction::NewContact {
            phone: phone_input.text().to_string(),
            name: Some(name.text().to_string()),
        });
        close.close();
    });
    phone.connect_map(|entry| {
        entry.grab_focus();
    });
    dialog.present();
}

/// Searchable participant list; picking one inserts the mention.
pub(super) fn show_mention_dialog(
    parent: &adw::ApplicationWindow,
    on_action: &DialogActionCallback,
    people: Vec<(String, String, Option<String>)>,
    avatars: &std::collections::HashMap<String, std::path::PathBuf>,
) {
    let dialog = adw::Dialog::builder()
        .title("Mention")
        .content_width(380)
        .content_height(520)
        .build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search participants")
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(6)
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(6)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .valign(gtk::Align::Start)
        .build();
    list.set_placeholder(Some(
        &gtk::Label::builder()
            .label("No participants match this search.")
            .margin_top(18)
            .margin_bottom(18)
            .css_classes(["dim-label"])
            .build(),
    ));
    let rows = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    // Photos are decoded in small idle batches so a large group opens at once.
    let mut photos = Vec::new();
    for (id, name, phone) in people {
        let row = adw::ActionRow::builder()
            .title(&name)
            .use_markup(false)
            .title_lines(1)
            .activatable(true)
            .build();
        let digits = phone
            .as_deref()
            .unwrap_or_default()
            .replace(|c: char| !c.is_ascii_digit(), "");
        if let Some(phone) = phone.filter(|phone| phone != &name) {
            row.set_subtitle(&phone);
        }
        let avatar = adw::Avatar::new(32, Some(&name), true);
        if let Some(path) = avatars.get(&id) {
            photos.push((avatar.downgrade(), path.clone()));
        }
        row.add_prefix(&avatar);
        rows.borrow_mut()
            .push((row.clone(), format!("{} {digits}", name.to_lowercase())));
        let (close, on_action) = (dialog.downgrade(), on_action.clone());
        row.connect_activated(move |_| {
            on_action(DialogAction::InsertMentionId(id.clone()));
            if let Some(close) = close.upgrade() {
                close.close();
            }
        });
        list.append(&row);
    }
    search.connect_search_changed(move |entry| {
        let needle = entry.text().trim().to_lowercase();
        for (row, key) in rows.borrow().iter() {
            row.set_visible(needle.is_empty() || key.contains(&needle));
        }
    });
    // Enter picks the first visible match.
    let first = list.clone();
    search.connect_activate(move |_| {
        let mut child = first.first_child();
        while let Some(widget) = child {
            if widget.is_visible()
                && let Some(row) = widget.downcast_ref::<adw::ActionRow>()
            {
                adw::prelude::ActionRowExt::activate(row);
                break;
            }
            child = widget.next_sibling();
        }
    });
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.add_top_bar(&search);
    view.set_content(Some(&scroll));
    dialog.set_child(Some(&view));
    dialog.set_focus(Some(&search));
    dialog.present(Some(parent));
    photos.reverse();
    gtk::glib::idle_add_local(move || {
        for _ in 0..8 {
            let Some((avatar, path)) = photos.pop() else {
                return gtk::glib::ControlFlow::Break;
            };
            if let Some(avatar) = avatar.upgrade() {
                avatar.set_custom_image(cached_texture(&path).as_ref());
            }
        }
        gtk::glib::ControlFlow::Continue
    });
}

/// Saved photo for a path, decoded once.
fn cached_texture(path: &std::path::Path) -> Option<gtk::gdk::Texture> {
    AVATAR_TEXTURES.with_borrow_mut(|cache| {
        if !cache.contains_key(path) {
            cache.insert(
                path.to_path_buf(),
                gtk::gdk::Texture::from_filename(path).ok()?,
            );
        }
        cache.get(path).cloned()
    })
}

/// Saved photo for a chat or participant, decoded once per path.
fn cached_avatar(
    avatars: &std::collections::HashMap<String, std::path::PathBuf>,
    id: &str,
) -> Option<gtk::gdk::Texture> {
    cached_texture(avatars.get(id)?)
}

/// Picks the chat to forward the selected message to.
pub(super) fn show_forward_dialog(
    parent: &adw::ApplicationWindow,
    on_action: &DialogActionCallback,
    summary: &str,
    chats: Vec<crate::model::Chat>,
    avatars: &std::collections::HashMap<String, std::path::PathBuf>,
) {
    let dialog = adw::Dialog::builder()
        .title("Forward To")
        .content_width(400)
        .content_height(560)
        .build();
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search chats")
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(6)
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(6)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    if !summary.is_empty() {
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(3)
            .margin_top(9)
            .margin_bottom(9)
            .margin_start(12)
            .margin_end(12)
            .build();
        card.append(
            &gtk::Label::builder()
                .label("Forwarding")
                .xalign(0.0)
                .css_classes(["caption-heading", "dim-label"])
                .build(),
        );
        card.append(
            &gtk::Label::builder()
                .label(summary)
                .xalign(0.0)
                .lines(2)
                .wrap(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
        );
        content.append(
            &gtk::Frame::builder()
                .child(&card)
                .css_classes(["card"])
                .build(),
        );
    }

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.set_placeholder(Some(
        &gtk::Label::builder()
            .label(if chats.is_empty() {
                "No chats to forward to."
            } else {
                "No chats match this search."
            })
            .wrap(true)
            .margin_top(18)
            .margin_bottom(18)
            .css_classes(["dim-label"])
            .build(),
    ));
    let rows = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    for chat in chats {
        let row = adw::ActionRow::builder()
            .title(&chat.name)
            .use_markup(false)
            .title_lines(1)
            .subtitle(forward_chat_detail(&chat))
            .activatable(true)
            .build();
        let avatar = adw::Avatar::new(32, Some(&chat.name), true);
        avatar.set_custom_image(cached_avatar(avatars, &chat.id).as_ref());
        row.add_prefix(&avatar);
        rows.borrow_mut()
            .push((row.clone(), forward_search_key(&chat)));
        let (close, on_action) = (dialog.clone(), on_action.clone());
        row.connect_activated(move |_| {
            on_action(DialogAction::ForwardSelected(chat.id.clone()));
            close.close();
        });
        list.append(&row);
    }
    search.connect_search_changed(move |entry| {
        let needle = entry.text().trim().to_lowercase();
        for (row, key) in rows.borrow().iter() {
            row.set_visible(needle.is_empty() || key.contains(&needle));
        }
    });
    content.append(&list);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.add_top_bar(&search);
    view.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build(),
    ));
    dialog.set_child(Some(&view));
    dialog.set_focus(Some(&search));
    dialog.present(Some(parent));
}

thread_local! {
    /// Small sticker previews by file, so picker rebuilds draw instantly.
    static STICKER_TEXTURES: std::cell::RefCell<
        std::collections::HashMap<std::path::PathBuf, gtk::gdk::Texture>,
    > = std::cell::RefCell::default();
}

/// Loads a sticker preview into `button` off the main thread, once mapped.
fn load_sticker_preview(button: &gtk::Button, path: &std::path::Path, size: i32) {
    let show = move |button: &gtk::Button, texture: Option<&gtk::gdk::Texture>| {
        let image = match texture {
            Some(texture) => gtk::Image::from_paintable(Some(texture)),
            None => gtk::Image::from_icon_name("image-missing-symbolic"),
        };
        image.set_pixel_size(size);
        button.set_child(Some(&image));
    };
    if let Some(texture) = STICKER_TEXTURES.with_borrow(|cache| cache.get(path).cloned()) {
        show(button, Some(&texture));
        return;
    }
    button.set_child(Some(&adw::Spinner::new()));
    let path = path.to_path_buf();
    let started = std::cell::Cell::new(false);
    button.connect_map(move |button| {
        if started.replace(true) {
            return;
        }
        let path = path.clone();
        let button = button.downgrade();
        gtk::glib::spawn_future_local(async move {
            let source = path.clone();
            // Decoded to a small RGBA preview; full-size WebP textures made the
            // picker slow and heavy.
            let decoded = gtk::gio::spawn_blocking(move || {
                let bytes = std::fs::read(&source).ok()?;
                let image = image::load_from_memory(&bytes)
                    .ok()?
                    .thumbnail(144, 144)
                    .to_rgba8();
                Some((image.width(), image.height(), image.into_raw()))
            })
            .await
            .ok()
            .flatten();
            let texture = decoded.map(|(width, height, rgba)| {
                gtk::gdk::MemoryTexture::new(
                    width as i32,
                    height as i32,
                    gtk::gdk::MemoryFormat::R8g8b8a8,
                    &gtk::glib::Bytes::from_owned(rgba),
                    width as usize * 4,
                )
                .upcast::<gtk::gdk::Texture>()
            });
            if let Some(texture) = &texture {
                STICKER_TEXTURES.with_borrow_mut(|cache| {
                    // ponytail: wholesale reset at ~40 MB of previews; an LRU if
                    // large libraries make reopening noticeably slower.
                    if cache.len() >= 500 {
                        cache.clear();
                    }
                    cache.insert(path, texture.clone());
                });
            }
            if let Some(button) = button.upgrade() {
                show(&button, texture.as_ref());
            }
        });
    });
}

/// Plays an animated sticker while the pointer is over its picker button,
/// and puts the still preview back when it leaves.
fn animate_sticker_on_hover(button: &gtk::Button, path: &std::path::Path) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let hovering = std::sync::Arc::new(AtomicBool::new(false));
    // Known after the first decode; still stickers are not decoded again.
    let still = std::rc::Rc::new(std::cell::Cell::new(false));
    let playing: std::rc::Rc<
        std::cell::RefCell<
            Option<(
                crate::native_media_widgets::StickerAnimation,
                gtk::gdk::Paintable,
            )>,
        >,
    > = Default::default();
    let hover = gtk::EventControllerMotion::new();
    {
        let (hovering, playing, path) = (hovering.clone(), playing.clone(), path.to_path_buf());
        let button = button.downgrade();
        hover.connect_enter(move |_, _, _| {
            if still.get() || hovering.swap(true, Ordering::AcqRel) {
                return;
            }
            let (hovering, playing, still, path) = (
                hovering.clone(),
                playing.clone(),
                still.clone(),
                path.clone(),
            );
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let current = hovering.clone();
                // ponytail: decoded again on every hover, and dropped on leave, so
                // an open picker holds one sticker's frames at most.
                let frames = gtk::gio::spawn_blocking(move || {
                    crate::native_media_widgets::decode_sticker_file(&path, || {
                        current.load(Ordering::Acquire)
                    })
                })
                .await
                .ok()
                .flatten();
                // Undecodable while still pointed at, not cancelled: treat as still.
                let Some(frames) = frames else {
                    still.set(hovering.load(Ordering::Acquire));
                    return;
                };
                if frames.len() < 2 {
                    still.set(true);
                    return;
                }
                let Some(image) = button
                    .upgrade()
                    .and_then(|button| button.child())
                    .and_downcast::<gtk::Image>()
                else {
                    return;
                };
                // A quick leave and return starts a second decode; the first
                // to finish plays.
                if !hovering.load(Ordering::Acquire) || playing.borrow().is_some() {
                    return;
                }
                let Some(preview) = image.paintable() else {
                    return;
                };
                let animation = crate::native_media_widgets::StickerAnimation::new(&image, frames);
                animation.play(None);
                *playing.borrow_mut() = Some((animation, preview));
            });
        });
    }
    // Closing the picker under the pointer sends no leave; stop there too.
    let rest = std::rc::Rc::new(move |button: &gtk::Button| {
        hovering.store(false, Ordering::Release);
        if let Some((animation, preview)) = playing.borrow_mut().take() {
            animation.stop();
            if let Some(image) = button.child().and_downcast::<gtk::Image>() {
                image.set_paintable(Some(&preview));
            }
        }
    });
    {
        let rest = rest.clone();
        hover.connect_leave(move |controller| {
            if let Some(button) = controller.widget().and_downcast::<gtk::Button>() {
                rest(&button);
            }
        });
    }
    button.connect_unmap(move |button| rest(button));
    button.add_controller(hover);
}

/// Shows recently used own stickers, followed by locally saved stickers.
pub(super) fn sticker_picker_content(
    recent: &[std::path::PathBuf],
    favorites: &[std::path::PathBuf],
    on_action: &DialogActionCallback,
) -> (gtk::Box, gtk::Stack) {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_size_request(380, 440);
    let stack = gtk::Stack::builder()
        .vexpand(true)
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    let stickers = recent
        .iter()
        .chain(favorites)
        .cloned()
        .fold(Vec::new(), |mut paths, path| {
            if !paths.contains(&path) {
                paths.push(path);
            }
            paths
        });
    if stickers.is_empty() {
        let empty = adw::StatusPage::builder()
            .icon_name("emoji-nature-symbolic")
            .title("No Stickers Yet")
            .description("Your recently used stickers appear here.")
            .vexpand(true)
            .build();
        empty.add_css_class("compact");
        content.append(&empty);
    } else {
        let grid = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(4)
            .max_children_per_line(4)
            .column_spacing(4)
            .row_spacing(4)
            .margin_start(8)
            .margin_end(8)
            .margin_top(8)
            .margin_bottom(8)
            .valign(gtk::Align::Start)
            .build();
        for path in &stickers {
            let button = gtk::Button::builder()
                .css_classes(["flat", "zaptide-sticker"])
                .tooltip_text("Send sticker")
                .build();
            button.update_property(&[gtk::accessible::Property::Label("Sticker")]);
            load_sticker_preview(&button, path, 72);
            animate_sticker_on_hover(&button, path);
            let path = path.clone();
            let on_action = on_action.clone();
            button.connect_clicked(move |_| on_action(DialogAction::SendSticker(path.clone())));
            grid.append(&button);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&grid)
            .build();
        stack.add_titled(&scroller, Some("recent"), "Stickers");
        content.append(&stack);
    }
    (content, stack)
}

pub(super) fn forward_search_key(chat: &crate::model::Chat) -> String {
    format!("{} {}", chat.name, chat.phone().unwrap_or_default()).to_lowercase()
}

pub(super) fn forward_chat_detail(chat: &crate::model::Chat) -> String {
    if chat.is_group() {
        format!("Group · {} participants", chat.participants.len())
    } else {
        chat.phone()
            .map(crate::util::phone)
            .unwrap_or_else(|| "Direct chat".into())
    }
}

/// Contact or group details: photo, name, number, and group members.
pub(super) fn show_chat_info_dialog(
    parent: &adw::ApplicationWindow,
    chat: &crate::model::Chat,
    contacts: &std::collections::HashMap<String, crate::model::Contact>,
    avatar: Option<&std::path::Path>,
    presence: Option<(bool, Option<i64>)>,
    chats: &[crate::model::Chat],
    avatars: &std::collections::HashMap<String, std::path::PathBuf>,
) -> Option<(String, adw::ActionRow)> {
    let name_of = |id: &str| {
        sender_label(
            contacts
                .get(id)
                .and_then(crate::model::Contact::display_name),
            id,
        )
    };
    let page = adw::PreferencesPage::new();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let picture = adw::Avatar::new(96, Some(&chat.name), true);
    let image = avatar.and_then(|path| gtk::gdk::Texture::from_filename(path).ok());
    picture.set_custom_image(image.as_ref());
    picture.set_margin_bottom(6);
    header.append(&picture);
    header.append(
        &gtk::Label::builder()
            .label(&chat.name)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["title-2"])
            .build(),
    );
    let detail = if chat.is_group() {
        format!("Group · {} participants", chat.participants.len())
    } else {
        chat.phone()
            .map(crate::util::phone)
            .unwrap_or_else(|| "Phone number unavailable".into())
    };
    header.append(
        &gtk::Label::builder()
            .label(&detail)
            .css_classes(["dim-label"])
            .build(),
    );
    let group = adw::PreferencesGroup::new();
    group.add(&header);
    page.add(&group);

    let group = adw::PreferencesGroup::new();
    if let Some(phone) = chat.phone() {
        let phone = crate::util::phone(phone);
        let row = adw::ActionRow::builder()
            .title("Phone")
            .use_markup(false)
            .subtitle(&phone)
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        let copy = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Copy phone number")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        copy.connect_clicked(move |button| {
            crate::native_portals::NativePortals::write_clipboard_text(&button.clipboard(), &phone);
            button.set_icon_name("object-select-symbolic");
        });
        row.add_suffix(&copy);
        group.add(&row);
    }
    let contact = contacts.get(&chat.id);
    let property = |title: &str, value: &str| {
        let row = adw::ActionRow::builder()
            .title(title)
            .use_markup(false)
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        // Set after `use-markup` is off: names such as "DNC&G" are not markup.
        row.set_subtitle(value);
        row
    };
    if let Some(saved) = contact
        .and_then(|contact| contact.full_name.as_deref())
        .filter(|saved| !saved.is_empty())
    {
        group.add(&property("Saved as", saved));
    }
    if let Some(push) = contact
        .and_then(|contact| contact.push_name.as_deref())
        .filter(|push| !push.is_empty())
    {
        group.add(&property("Name on WhatsApp", &format!("~{push}")));
    }
    let about = property("About", "");
    about.set_visible(false);
    if chat.phone().is_some() {
        group.add(&about);
    }
    match presence {
        Some((true, _)) => group.add(&property("Status", "Online")),
        Some((false, Some(at))) => {
            group.add(&property("Last seen", &crate::util::moment_stamp(at)))
        }
        _ => {}
    }
    if chat.phone().is_some() {
        page.add(&group);
    }

    let settings = adw::PreferencesGroup::new();
    let now = crate::util::now();
    settings.add(&property(
        "Notifications",
        match chat.muted_until {
            // Muting from the app stores `i64::MAX`, which has no date.
            Some(until) if until > now && !crate::util::moment_stamp(until).is_empty() => {
                format!("Muted until {}", crate::util::moment_stamp(until))
            }
            _ if chat.muted(now) => "Muted".to_owned(),
            _ => "On".to_owned(),
        }
        .as_str(),
    ));
    if let Some(seconds) = chat.ephemeral_expiration.filter(|seconds| *seconds > 0) {
        let label = match seconds {
            86_400 => "24 hours".to_owned(),
            604_800 => "7 days".to_owned(),
            7_776_000 => "90 days".to_owned(),
            other => format!("{other} seconds"),
        };
        settings.add(&property("Disappearing messages", &label));
    }
    let flags: Vec<&str> = [(chat.pinned, "Pinned"), (chat.archived, "Archived")]
        .into_iter()
        .filter_map(|(on, label)| on.then_some(label))
        .collect();
    if !flags.is_empty() {
        settings.add(&property("Chat", &flags.join(" · ")));
    }
    page.add(&settings);

    if !chat.is_group() {
        let mut shared: Vec<&crate::model::Chat> = chats
            .iter()
            .filter(|other| other.is_group() && other.participants.contains(&chat.id))
            .collect();
        if !shared.is_empty() {
            shared.sort_by_cached_key(|other| other.name.to_lowercase());
            let group = adw::PreferencesGroup::builder()
                .title(format!("Groups in common ({})", shared.len()))
                .build();
            for other in shared {
                let row = adw::ActionRow::builder()
                    .title(&other.name)
                    .use_markup(false)
                    .title_lines(1)
                    .build();
                row.set_subtitle(&format!("{} participants", other.participants.len()));
                let photo = adw::Avatar::new(32, Some(&other.name), true);
                photo.set_custom_image(cached_avatar(avatars, &other.id).as_ref());
                row.add_prefix(&photo);
                group.add(&row);
            }
            page.add(&group);
        }
    }

    if chat.is_group() && !chat.participants.is_empty() {
        let group = adw::PreferencesGroup::builder()
            .title("Participants")
            .build();
        let mut members: Vec<_> = chat
            .participants
            .iter()
            .map(|id| (name_of(id), id))
            .collect();
        members.sort_by_cached_key(|(name, _)| name.to_lowercase());
        for (name, id) in members {
            let row = adw::ActionRow::builder()
                .title(&name)
                .use_markup(false)
                .build();
            if let Some(phone) = crate::model::phone_of(id).map(crate::util::phone)
                && phone != name
            {
                row.set_subtitle(&phone);
            }
            row.add_prefix(&adw::Avatar::new(32, Some(&name), true));
            group.add(&row);
        }
        page.add(&group);
    }

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title(if chat.is_group() {
            "Group Info"
        } else {
            "Contact Info"
        })
        .content_width(400)
        .content_height(if chat.is_group() { 600 } else { 560 })
        .child(&view)
        .build();
    dialog.present(Some(parent));
    chat.phone().map(|_| (chat.id.clone(), about))
}
