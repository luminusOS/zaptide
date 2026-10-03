use super::*;
use crate::contact_cards::{ContactCard, MAX_VCARD_BYTES};
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
pub struct ShareTarget {
    chat: String,
    name: String,
    generation: u64,
    quoting: Option<String>,
}

impl std::fmt::Debug for ShareTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ShareTarget(..)")
    }
}

impl NativeApplication {
    pub(super) fn show_contact_picker(&mut self, sender: &ComponentSender<Self>) {
        if !self.can_attach() || !self.link.is_connected() {
            self.toast("Choose a writable chat while connected to share a contact");
            return;
        }
        let Some(chat) = self
            .active_chat
            .as_ref()
            .and_then(|id| self.chat_snapshots.iter().find(|chat| &chat.id == id))
        else {
            return;
        };
        let target = ShareTarget {
            chat: chat.id.clone(),
            name: chat.name.clone(),
            generation: self.contact_share_generation,
            quoting: self
                .reply_to
                .as_ref()
                .filter(|(id, _)| id == &chat.id)
                .map(|(_, message)| message.clone()),
        };
        let contacts = new_chat_contacts(&self.contacts)
            .into_iter()
            .filter_map(|(id, name, _)| ContactCard::from_saved(&id, &name))
            .collect();
        show_picker(&self.window, target, contacts, true, sender.input_sender());
    }

    fn valid_contact_target(&self, target: &ShareTarget) -> bool {
        valid_target(
            target,
            self.contact_share_generation,
            self.link.is_connected(),
            &self.chat_snapshots,
        )
    }

    pub(super) fn share_contact(&mut self, target: ShareTarget, contact: ContactCard) {
        if !self.valid_contact_target(&target) {
            self.toast("Contact not sent: the conversation or linked account changed");
            return;
        }
        if let Some(backend) = &self.backend {
            backend.send(crate::backend::Command::SendContact {
                chat: target.chat,
                contact,
                quoting: target.quoting,
            });
            self.status = "Sending contact".into();
        } else {
            self.toast("Backend unavailable");
        }
    }

    pub(super) fn import_contact(&mut self, target: ShareTarget, sender: &ComponentSender<Self>) {
        if !self.valid_contact_target(&target) {
            return;
        }
        if !self.portal_requests.borrow().is_empty() {
            self.toast("Finish the active file chooser first");
            return;
        }
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Contact cards (.vcf)"));
        filter.add_mime_type("text/vcard");
        filter.add_mime_type("text/x-vcard");
        filter.add_pattern("*.vcf");
        filter.add_pattern("*.VCF");
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let slot = Rc::new(std::cell::Cell::new(None));
        let callback_slot = slot.clone();
        let request = self.portals.open_file(
            Some(&self.window),
            "Choose a contact card",
            &filter,
            move |result| {
                if let Some(id) = callback_slot.get() {
                    requests.borrow_mut().remove(&id);
                }
                match result {
                    Ok(Some(file)) => {
                        gtk::glib::spawn_future_local(async move {
                            let result = read_contacts(&file).await;
                            input.input(Input::ContactFileReady { target, result });
                        });
                    }
                    Ok(None) => {}
                    Err(_) => input.input(Input::ContactFileReady {
                        target,
                        result: Err("Could not open the contact file chooser".into()),
                    }),
                }
            },
        );
        if let Some(id) = request {
            slot.set(Some(id));
            self.portal_requests.borrow_mut().insert(id);
        } else {
            self.toast("Could not open the contact file chooser");
        }
    }

    pub(super) fn contact_file_ready(
        &mut self,
        target: ShareTarget,
        result: Result<Vec<ContactCard>, String>,
        sender: &ComponentSender<Self>,
    ) {
        if !self.valid_contact_target(&target) {
            return;
        }
        match result {
            Ok(contacts) => {
                show_picker(&self.window, target, contacts, false, sender.input_sender());
            }
            Err(error) => self.toast(&error),
        }
    }

    pub(super) fn open_contact(&mut self, contact: ContactCard, sender: &ComponentSender<Self>) {
        if !self.portal_requests.borrow().is_empty() {
            self.toast("Finish the active file chooser first");
            return;
        }
        let input = sender.clone();
        let window = self.window.clone();
        let requests = self.portal_requests.clone();
        let slot = Rc::new(std::cell::Cell::new(None));
        let callback_slot = slot.clone();
        let request = self.portals.save_bytes(
            Some(&self.window),
            "Save contact to open in Contacts",
            "contact.vcf",
            contact.vcard.into_bytes(),
            move |result| {
                if let Some(id) = callback_slot.get() {
                    requests.borrow_mut().remove(&id);
                }
                match result {
                    Ok(Some(file)) => gtk::FileLauncher::new(Some(&file)).launch(
                        Some(&window),
                        gtk::gio::Cancellable::NONE,
                        move |result| {
                            input.input(Input::ContactActionFinished(result.map_err(|_| {
                                "Contact saved, but no contacts app could open it".into()
                            })));
                        },
                    ),
                    Ok(None) => {}
                    Err(_) => input.input(Input::ContactActionFinished(Err(
                        "Could not save contact".into(),
                    ))),
                }
            },
        );
        if let Some(id) = request {
            slot.set(Some(id));
            self.portal_requests.borrow_mut().insert(id);
        } else {
            self.toast("Could not open the contact save dialog");
        }
    }
}

fn valid_target(
    target: &ShareTarget,
    generation: u64,
    connected: bool,
    chats: &[crate::model::Chat],
) -> bool {
    target.generation == generation
        && connected
        && chats
            .iter()
            .any(|chat| chat.id == target.chat && chat.can_send())
}

async fn read_contacts(file: &gtk::gio::File) -> Result<Vec<ContactCard>, String> {
    let stream = file
        .read_future(gtk::glib::Priority::DEFAULT)
        .await
        .map_err(|_| "Could not read contact file".to_owned())?;
    let mut bytes = Vec::new();
    loop {
        let chunk = stream
            .read_bytes_future(
                (MAX_VCARD_BYTES + 1 - bytes.len()).min(8192),
                gtk::glib::Priority::DEFAULT,
            )
            .await
            .map_err(|_| "Could not read contact file".to_owned())?;
        if chunk.is_empty() {
            break;
        }
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_VCARD_BYTES {
            return Err("Contact file is too large (maximum 256 KiB)".into());
        }
    }
    let text =
        String::from_utf8(bytes).map_err(|_| "Contact file must use UTF-8 text".to_owned())?;
    let contacts = crate::contact_cards::parse(&text);
    if contacts.is_empty() {
        Err("No valid contacts found in this vCard file".into())
    } else {
        Ok(contacts)
    }
}

fn show_picker(
    parent: &adw::ApplicationWindow,
    target: ShareTarget,
    contacts: Vec<ContactCard>,
    allow_import: bool,
    sender: &relm4::Sender<Input>,
) -> adw::Dialog {
    let dialog = adw::Dialog::builder()
        .title("Share Contact")
        .content_width(400)
        .content_height(540)
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(
            "Share Contact",
            &format!("To {}", target.name),
        ))
        .build();
    let import_button = |label: &str| {
        let button = gtk::Button::with_label(label);
        let (input, target, close) = (sender.clone(), target.clone(), dialog.downgrade());
        button.connect_clicked(move |_| {
            if let Some(dialog) = close.upgrade() {
                dialog.close();
            }
            input.emit(Input::ImportContact(target.clone()));
        });
        button
    };
    if allow_import && !contacts.is_empty() {
        let import = import_button("");
        import.set_icon_name("document-open-symbolic");
        import.set_tooltip_text(Some("Import vCard File…"));
        import.update_property(&[gtk::accessible::Property::Label("Import vCard File…")]);
        header.pack_start(&import);
    }
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search names or phone numbers")
        .margin_start(12)
        .margin_end(12)
        .margin_bottom(6)
        .visible(!contacts.is_empty())
        .build();
    search.update_property(&[gtk::accessible::Property::Label("Search contacts")]);
    let no_contacts = adw::StatusPage::builder()
        .icon_name("avatar-default-symbolic")
        .title("No Saved Contacts")
        .description("Saved contacts appear here after syncing with your phone.")
        .css_classes(["compact"])
        .build();
    if allow_import {
        let import = import_button("Import vCard File…");
        import.set_halign(gtk::Align::Center);
        import.add_css_class("pill");
        no_contacts.set_child(Some(&import));
    }
    let no_results = adw::StatusPage::builder()
        .icon_name("edit-find-symbolic")
        .title("No Results Found")
        .description("Try a different name or phone number.")
        .css_classes(["compact"])
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .valign(gtk::Align::Start)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(12)
        .css_classes(["boxed-list"])
        .build();
    let has_contacts = !contacts.is_empty();
    for contact in contacts {
        let subtitle = contact
            .phones
            .iter()
            .map(|p| p.number.as_str())
            .collect::<Vec<_>>()
            .join(" · ");
        let row = adw::ActionRow::builder()
            .title(&contact.name)
            .subtitle(&subtitle)
            .use_markup(false)
            .title_lines(1)
            .subtitle_lines(1)
            .activatable(true)
            .build();
        row.add_prefix(&adw::Avatar::new(32, Some(&contact.name), true));
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let (close, parent, target, input) = (
            dialog.downgrade(),
            parent.clone(),
            target.clone(),
            sender.clone(),
        );
        row.connect_activated(move |_| {
            show_preview(&parent, &target, &contact, &input, close.clone());
        });
        list.append(&row);
    }
    let query = Rc::new(RefCell::new(String::new()));
    let filter_query = query.clone();
    list.set_filter_func(move |row| row_matches(row, &filter_query.borrow()));
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&adw::Clamp::builder().child(&list).build())
        .build();
    let stack = gtk::Stack::builder().vexpand(true).build();
    stack.add_named(&scroll, Some("list"));
    stack.add_named(&no_contacts, Some("no-contacts"));
    stack.add_named(&no_results, Some("no-results"));
    stack.set_visible_child_name(if has_contacts { "list" } else { "no-contacts" });
    let (filtered, pages) = (list.downgrade(), stack.downgrade());
    search.connect_changed(move |entry| {
        *query.borrow_mut() = entry.text().trim().to_lowercase();
        let (Some(list), Some(stack)) = (filtered.upgrade(), pages.upgrade()) else {
            return;
        };
        list.invalidate_filter();
        let mut any = false;
        let mut index = 0;
        while let Some(row) = list.row_at_index(index) {
            any |= row_matches(&row, &query.borrow());
            index += 1;
        }
        stack.set_visible_child_name(if any { "list" } else { "no-results" });
    });
    let keyboard_list = list.downgrade();
    search.connect_activate(move |_| {
        if let Some(list) = keyboard_list.upgrade() {
            let mut index = 0;
            while let Some(row) = list.row_at_index(index) {
                if row.is_child_visible() && row.is_visible() {
                    if let Some(row) = row.downcast_ref::<adw::ActionRow>() {
                        adw::prelude::ActionRowExt::activate(row);
                    }
                    break;
                }
                index += 1;
            }
        }
    });
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.add_top_bar(&search);
    view.set_content(Some(&stack));
    dialog.set_child(Some(&view));
    search.set_key_capture_widget(Some(&view));
    if has_contacts {
        dialog.set_focus(Some(&search));
    }
    dialog.present(Some(parent));
    dialog
}

/// Matches names and numbers; digit-only queries ignore phone punctuation.
fn row_matches(row: &gtk::ListBoxRow, query: &str) -> bool {
    let Some(row) = row.downcast_ref::<adw::ActionRow>() else {
        return false;
    };
    let key = format!("{} {}", row.title(), row.subtitle().unwrap_or_default()).to_lowercase();
    key.contains(query) || {
        let digits = query
            .chars()
            .filter(char::is_ascii_digit)
            .collect::<String>();
        !digits.is_empty()
            && query
                .chars()
                .all(|c| c.is_ascii_digit() || "+ ()-.".contains(c))
            && key
                .chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
                .contains(&digits)
    }
}

fn show_preview(
    parent: &adw::ApplicationWindow,
    target: &ShareTarget,
    contact: &ContactCard,
    sender: &relm4::Sender<Input>,
    picker: gtk::glib::WeakRef<adw::Dialog>,
) -> adw::Dialog {
    let dialog = adw::Dialog::builder()
        .title("Send Contact")
        .content_width(400)
        .build();
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(
            "Send Contact",
            &format!("To {}", target.name),
        ))
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let send = gtk::Button::with_label("Send");
    send.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&send);
    let close = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
    });
    let (close, input, target, contact_send) = (
        dialog.downgrade(),
        sender.clone(),
        target.clone(),
        contact.clone(),
    );
    send.connect_clicked(move |button| {
        button.set_sensitive(false);
        input.emit(Input::SendContact {
            target: target.clone(),
            contact: contact_send.clone(),
        });
        if let Some(dialog) = close.upgrade() {
            dialog.close();
        }
        if let Some(picker) = picker.upgrade() {
            picker.close();
        }
    });
    let page = crate::native_media_widgets::contact_preview(contact);
    page.set_margin_top(24);
    page.set_margin_bottom(24);
    page.set_margin_start(24);
    page.set_margin_end(24);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&page)
        .build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&scroll));
    dialog.set_child(Some(&view));
    dialog.set_default_widget(Some(&send));
    dialog.set_focus(Some(&send));
    dialog.present(Some(parent));
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a display"]
    fn render_contact_picker_and_preview() {
        gtk::init().unwrap();
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("dev.luminusos.ZapTide.ContactFixture")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let context = gtk::glib::MainContext::default();
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .default_width(440)
            .default_height(640)
            .build();
        let theme = gtk::CssProvider::new();
        apply_theme(&crate::settings::Settings::default(), &theme);
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &theme,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        window.set_content(Some(&attach_tile(
            "avatar-default-symbolic",
            "Contact",
            "contact",
        )));
        window.present();
        let dir = std::path::PathBuf::from(std::env::var("RENDER_DIR").unwrap());
        let settle = || {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(400);
            while std::time::Instant::now() < end {
                context.iteration(false);
            }
        };
        let capture = |name: &str| {
            // Xvfb occasionally has not drawn the first frame yet; retry briefly.
            let node = (0..10)
                .find_map(|_| {
                    settle();
                    let snapshot = gtk::Snapshot::new();
                    gtk::WidgetPaintable::new(Some(&window)).snapshot(
                        &snapshot,
                        f64::from(window.width()),
                        f64::from(window.height()),
                    );
                    snapshot.to_node()
                })
                .expect("window never rendered");
            let texture = window
                .native()
                .unwrap()
                .renderer()
                .unwrap()
                .render_texture(node, None);
            let path = dir.join(format!("{name}.png"));
            std::fs::write(&path, texture.save_to_png_bytes()).unwrap();
            assert!(path.is_file());
        };
        fn descendants<T: IsA<gtk::Widget> + gtk::glib::object::IsClass>(
            widget: &gtk::Widget,
        ) -> Vec<T> {
            let mut result = Vec::new();
            if let Ok(item) = widget.clone().downcast::<T>() {
                result.push(item);
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                result.extend(descendants::<T>(&widget));
                child = widget.next_sibling();
            }
            result
        }
        let (input, received) = relm4::channel::<Input>();
        let target = ShareTarget {
            chat: "15555550125@s.whatsapp.net".into(),
            name: "Fixture destination".into(),
            generation: 3,
            quoting: None,
        };
        let contacts = vec![
            ContactCard::from_saved("15555550123@s.whatsapp.net", "Ada Example").unwrap(),
            ContactCard::from_saved("15555550124@s.whatsapp.net", "Éva Example").unwrap(),
        ];
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
        capture("contact-attach-light");
        let picker = show_picker(&window, target.clone(), contacts.clone(), true, &input);
        capture("contact-picker-light");
        let search = descendants::<gtk::SearchEntry>(picker.upcast_ref())
            .pop()
            .unwrap();
        // No debounce/main-loop wait between typing and Enter.
        search.set_text("Éva");
        search.emit_by_name::<()>("activate", &[]);
        let preview = window.visible_dialog().unwrap();
        assert_eq!(preview.title(), "Send Contact");
        let labels = descendants::<gtk::Label>(preview.upcast_ref());
        assert!(labels.iter().any(|label| label.label() == "Éva Example"));
        assert!(!labels.iter().any(|label| label.label() == "Ada Example"));
        capture("contact-preview-light");
        preview.close();
        settle();
        assert_eq!(window.visible_dialog().unwrap(), picker);
        assert_eq!(search.text(), "Éva");
        search.set_text("no matching contact");
        search.emit_by_name::<()>("activate", &[]);
        assert_eq!(window.visible_dialog().unwrap(), picker);
        let pages = descendants::<gtk::Stack>(picker.upcast_ref())
            .pop()
            .unwrap();
        assert_eq!(pages.visible_child_name().as_deref(), Some("no-results"));
        capture("contact-picker-no-results-light");
        search.set_text("");
        assert_eq!(pages.visible_child_name().as_deref(), Some("list"));
        picker.close();
        settle();
        let empty = show_picker(&window, target.clone(), Vec::new(), true, &input);
        capture("contact-picker-empty-light");
        empty.close();
        settle();
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
        let preview = show_preview(
            &window,
            &target,
            &contacts[0],
            &input,
            gtk::glib::WeakRef::new(),
        );
        capture("contact-preview-dark");
        preview
            .default_widget()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        let result = context.block_on(received.recv()).unwrap();
        assert!(
            matches!(result, Input::SendContact { target: destination, contact } if destination.chat == target.chat && contact == contacts[0])
        );
        window.close();
    }

    #[test]
    fn captured_destination_is_not_retargeted_and_stale_or_locked_targets_are_rejected() {
        let target = ShareTarget {
            chat: "15555550123@s.whatsapp.net".into(),
            name: "Fixture".into(),
            generation: 3,
            quoting: None,
        };
        let mut chats = vec![
            crate::model::Chat::new(target.chat.clone(), "Fixture".into()),
            crate::model::Chat::new("15555550124@s.whatsapp.net".into(), "Other".into()),
        ];
        assert!(valid_target(&target, 3, true, &chats));
        assert!(!valid_target(&target, 4, true, &chats));
        assert!(!valid_target(&target, 3, false, &chats));
        chats[0].locked = true;
        assert!(!valid_target(&target, 3, true, &chats));
        assert!(!valid_target(&target, 3, true, &chats[1..]));
    }

    #[test]
    fn imports_vcards_asynchronously_and_rejects_oversized_or_invalid_files() {
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                let dir = tempfile::tempdir().unwrap();
                let file = dir.path().join("fixture.vcf");
                let card =
                    ContactCard::from_saved("15555550123@s.whatsapp.net", "Éva Example").unwrap();
                std::fs::write(&file, card.vcard.as_bytes()).unwrap();
                let handle = gtk::gio::File::for_path(&file);
                assert_eq!(
                    context.block_on(read_contacts(&handle)).unwrap(),
                    vec![card]
                );
                std::fs::write(&file, vec![b'x'; MAX_VCARD_BYTES + 1]).unwrap();
                assert!(
                    context
                        .block_on(read_contacts(&handle))
                        .unwrap_err()
                        .contains("too large")
                );
                std::fs::write(&file, b"not a contact").unwrap();
                assert!(context.block_on(read_contacts(&handle)).is_err());
                std::fs::write(&file, [0xff]).unwrap();
                assert!(
                    context
                        .block_on(read_contacts(&handle))
                        .unwrap_err()
                        .contains("UTF-8")
                );
            })
            .unwrap();
    }
}
