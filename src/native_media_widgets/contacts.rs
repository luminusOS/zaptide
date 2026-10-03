use super::*;
use crate::contact_cards::ContactCard;
use adw::prelude::ActionRowExt;
use std::rc::Rc;

pub(super) fn append_contact_cards(
    root: &gtk::Box,
    fallback_name: &str,
    vcard: &str,
    on_action: Rc<dyn Fn(NativeMediaAction)>,
) {
    let contacts = crate::contact_cards::parse(vcard);
    if contacts.is_empty() {
        root.append(&header(
            fallback_name,
            Some("Contact details unavailable"),
            None,
        ));
        return;
    }
    for (index, contact) in contacts.iter().enumerate() {
        if index > 0 {
            let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
            separator.set_margin_top(6);
            separator.set_margin_bottom(6);
            root.append(&separator);
        }
        root.append(&contact_card(contact, on_action.clone()));
    }
}

/// Avatar, name and an optional dimmed detail line, like an `adw::ActionRow`.
fn header(name: &str, detail: Option<&str>, suffix: Option<&gtk::Button>) -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let avatar = adw::Avatar::new(40, Some(name), true);
    avatar.set_valign(gtk::Align::Center);
    heading.append(&avatar);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.set_valign(gtk::Align::Center);
    let title = add_label(&text, name);
    title.set_max_width_chars(30);
    title.add_css_class("heading");
    if let Some(detail) = detail {
        let detail = add_label(&text, detail);
        detail.set_max_width_chars(30);
        detail.add_css_class("dim-label");
    }
    heading.append(&text);
    if let Some(suffix) = suffix {
        heading.append(suffix);
    }
    card.append(&heading);
    card
}

fn copy_button(number: &str, on_action: &Rc<dyn Fn(NativeMediaAction)>) -> gtk::Button {
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy phone number")
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    copy.update_property(&[gtk::accessible::Property::Label(&format!("Copy {number}"))]);
    let (action, number) = (on_action.clone(), number.to_owned());
    copy.connect_clicked(move |_| action(NativeMediaAction::CopyContactPhone(number.clone())));
    copy
}

/// Centered summary for the send confirmation: who is shared and which numbers.
pub(crate) fn contact_preview(contact: &ContactCard) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let avatar = adw::Avatar::new(80, Some(&contact.name), true);
    avatar.set_halign(gtk::Align::Center);
    page.append(&avatar);
    page.append(
        &gtk::Label::builder()
            .label(&contact.name)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .justify(gtk::Justification::Center)
            .max_width_chars(30)
            .css_classes(["title-2"])
            .build(),
    );
    let phones = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .margin_top(6)
        .build();
    for phone in &contact.phones {
        let row = adw::ActionRow::builder()
            .title(&phone.number)
            .use_markup(false)
            .build();
        if phone.whatsapp_id.is_some() {
            row.set_subtitle("On WhatsApp");
        }
        row.add_prefix(&gtk::Image::from_icon_name("phone-symbolic"));
        phones.append(&row);
    }
    if contact.phones.is_empty() {
        phones.append(&adw::ActionRow::builder().title("No phone number").build());
    }
    page.append(&phones);
    page
}

/// Flat card action whose label wraps, so side-by-side actions fit narrow bubbles.
fn action_button(label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .child(
            &gtk::Label::builder()
                .label(label)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .justify(gtk::Justification::Center)
                .max_width_chars(30)
                .build(),
        )
        .hexpand(true)
        .build();
    button.add_css_class("flat");
    button
}

/// Received contact: header with numbers, then Message and Add to Contacts.
fn contact_card(contact: &ContactCard, on_action: Rc<dyn Fn(NativeMediaAction)>) -> gtk::Box {
    let card = match contact.phones.as_slice() {
        [] => header(&contact.name, Some("No phone number"), None),
        [phone] => header(
            &contact.name,
            Some(&phone.number),
            Some(&copy_button(&phone.number, &on_action)),
        ),
        phones => {
            let card = header(&contact.name, None, None);
            for phone in phones {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                // Align numbers with the name, past the 40 px avatar and spacing.
                row.set_margin_start(52);
                let number = add_label(&row, &phone.number);
                number.set_hexpand(true);
                number.set_max_width_chars(30);
                number.add_css_class("dim-label");
                row.append(&copy_button(&phone.number, &on_action));
                card.append(&row);
            }
            card
        }
    };
    let whatsapp_numbers = contact
        .phones
        .iter()
        .filter_map(|p| p.whatsapp_id.as_ref())
        .collect::<std::collections::HashSet<_>>()
        .len();
    let mut messages = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for phone in &contact.phones {
        if let Some(id) = &phone.whatsapp_id
            && ids.insert(id.clone())
        {
            let label = if whatsapp_numbers > 1 {
                format!("Message {}", phone.number)
            } else {
                "Message".to_owned()
            };
            let button = action_button(&label);
            button.update_property(&[gtk::accessible::Property::Label(&format!(
                "Message {} at {}",
                contact.name, phone.number
            ))]);
            let (action, id, name) = (on_action.clone(), id.clone(), contact.name.clone());
            button.connect_clicked(move |_| {
                action(NativeMediaAction::MessageContact {
                    id: id.clone(),
                    name: name.clone(),
                })
            });
            messages.push(button);
        }
    }
    let open = action_button("Add to Contacts…");
    // Accessible name starts with the visible label for voice control.
    open.update_property(&[gtk::accessible::Property::Label(&format!(
        "Add to Contacts: {}",
        contact.name
    ))]);
    open.set_tooltip_text(Some(
        "Save a vCard and open it in your default contacts app",
    ));
    let contact = contact.clone();
    open.connect_clicked(move |_| on_action(NativeMediaAction::OpenContact(contact.clone())));
    card.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    if let [message] = messages.as_slice() {
        let divider = gtk::Separator::new(gtk::Orientation::Vertical);
        divider.set_margin_top(6);
        divider.set_margin_bottom(6);
        let actions = gtk::CenterBox::new();
        actions.set_start_widget(Some(message));
        actions.set_center_widget(Some(&divider));
        actions.set_end_widget(Some(&open));
        card.append(&actions);
    } else {
        let actions = gtk::Box::new(gtk::Orientation::Vertical, 0);
        for message in &messages {
            actions.append(message);
        }
        actions.append(&open);
        card.append(&actions);
    }
    card
}
