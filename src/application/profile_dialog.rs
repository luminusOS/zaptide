//! The signed-in account's own profile: picture, name, and About text.

use crate::backend::{PictureEdit, ProfileEdit};
use crate::i18n::gettext;
use crate::native_preferences::PreferenceChange;
use relm4::adw::prelude::*;
use relm4::{adw, gtk};
use std::{cell::RefCell, rc::Rc};

/// WhatsApp's own limits, so a refusal is explained before it is sent.
const NAME_LIMIT: usize = 25;
const ABOUT_LIMIT: usize = 139;

pub(super) struct ProfileInit {
    pub(super) name: String,
    pub(super) picture: Option<std::path::PathBuf>,
    pub(super) own_id: String,
    pub(super) send_read_receipts: bool,
    pub(super) send_typing: bool,
    pub(super) link_previews: bool,
    pub(super) chat_lock_configured: bool,
}

/// Settings kept per account: they apply at once and are not part of Save.
fn privacy_group(
    init: &ProfileInit,
    on_setting: Rc<dyn Fn(PreferenceChange)>,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title(gettext("Privacy"))
        .build();
    type Switch = (String, String, bool, fn(bool) -> PreferenceChange);
    let switches: [Switch; 3] = [
        (
            gettext("Send read receipts"),
            gettext("Let contacts know when messages are read"),
            init.send_read_receipts,
            PreferenceChange::SetSendReadReceipts,
        ),
        (
            gettext("Send typing status"),
            gettext("Let contacts know while you are typing"),
            init.send_typing,
            PreferenceChange::SetSendTyping,
        ),
        (
            gettext("Link previews"),
            gettext("Show a preview of the links you send"),
            init.link_previews,
            PreferenceChange::SetLinkPreviews,
        ),
    ];
    for (title, subtitle, active, change) in switches {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .active(active)
            .build();
        let on_setting = on_setting.clone();
        row.connect_active_notify(move |row| on_setting(change(row.is_active())));
        group.add(&row);
    }

    let lock_row = adw::ActionRow::builder()
        .title(gettext("Locked chats code"))
        .subtitle(gettext("Write-only. The saved code is never shown."))
        .build();
    let set = gtk::Button::builder().valign(gtk::Align::Center).build();
    let clear = gtk::Button::builder()
        .label(gettext("Remove…"))
        .valign(gtk::Align::Center)
        .css_classes(["destructive-action"])
        .build();
    set.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Set locked chats code",
    ))]);
    set.set_tooltip_text(Some(&gettext("Set or change locked chats code")));
    clear.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Remove locked chats code",
    ))]);
    clear.set_tooltip_text(Some(&gettext("Remove locked chats code")));
    let show_lock: Rc<dyn Fn(bool)> = {
        let (set, clear) = (set.clone(), clear.clone());
        Rc::new(move |configured| {
            set.set_label(&if configured {
                gettext("Change…")
            } else {
                gettext("Set…")
            });
            clear.set_visible(configured);
        })
    };
    show_lock(init.chat_lock_configured);
    {
        let (on_setting, show_lock) = (on_setting.clone(), show_lock.clone());
        set.connect_clicked(move |button| {
            let (on_setting, show_lock) = (on_setting.clone(), show_lock.clone());
            crate::native_preferences::present_secret_entry(
                button,
                &gettext("Set locked chats code"),
                &gettext("Enter a code. The saved code cannot be viewed."),
                &gettext("Locked chats code"),
                move |text| {
                    on_setting(PreferenceChange::SetChatLockCode(Some(text)));
                    show_lock(true);
                },
            );
        });
    }
    clear.connect_clicked(move |button| {
        let alert = adw::AlertDialog::new(
            Some(&gettext("Remove locked chats code?")),
            Some(&gettext("Locked chats will no longer require this code.")),
        );
        alert.add_response("cancel", &gettext("Cancel"));
        alert.add_response("remove", &gettext("Remove"));
        alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        alert.set_close_response("cancel");
        alert.set_default_response(Some("cancel"));
        let (on_setting, show_lock) = (on_setting.clone(), show_lock.clone());
        alert.connect_response(None, move |_, response| {
            if response == "remove" {
                on_setting(PreferenceChange::SetChatLockCode(None));
                show_lock(false);
            }
        });
        alert.present(Some(button));
    });
    lock_row.add_suffix(&set);
    lock_row.add_suffix(&clear);
    group.add(&lock_row);
    group
}

/// Why a name or About text cannot be saved, if it cannot.
fn problem(name: &str, about: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        Some("The name cannot be empty")
    } else if name.chars().count() > NAME_LIMIT {
        Some("The name is too long")
    } else if about.chars().count() > ABOUT_LIMIT {
        Some("The About text is too long")
    } else {
        None
    }
}

struct State {
    name: String,
    about: String,
    about_loaded: bool,
    picture: PictureEdit,
}

pub(super) struct ProfileDialog {
    own_id: String,
    dialog: adw::Dialog,
    about: adw::EntryRow,
    content: gtk::Widget,
    state: Rc<RefCell<State>>,
    refresh: Rc<dyn Fn()>,
}

impl ProfileDialog {
    pub(super) fn present(
        parent: &adw::ApplicationWindow,
        init: ProfileInit,
        on_save: impl Fn(ProfileEdit) + 'static,
        on_setting: impl Fn(PreferenceChange) + 'static,
    ) -> Self {
        let on_setting: Rc<dyn Fn(PreferenceChange)> = Rc::new(on_setting);
        let state = Rc::new(RefCell::new(State {
            name: init.name.clone(),
            about: String::new(),
            about_loaded: false,
            picture: PictureEdit::Keep,
        }));
        let avatar = adw::Avatar::new(96, Some(&init.name), true);
        avatar.set_halign(gtk::Align::Center);
        avatar.set_custom_image(
            init.picture
                .as_deref()
                .and_then(|path| gtk::gdk::Texture::from_filename(path).ok())
                .as_ref(),
        );
        let change = gtk::Button::builder()
            .label(gettext("Change Photo…"))
            .css_classes(["pill"])
            .build();
        let remove = gtk::Button::builder()
            .label(gettext("Remove"))
            .css_classes(["pill", "destructive-action"])
            .visible(init.picture.is_some())
            .build();
        let photo_buttons = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        photo_buttons.append(&change);
        photo_buttons.append(&remove);

        let name = adw::EntryRow::builder().title(gettext("Name")).build();
        name.set_text(&init.name);
        let about = adw::EntryRow::builder().title(gettext("About")).build();
        let group = adw::PreferencesGroup::new();
        group.add(&name);
        group.add(&about);
        let hint = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label", "caption"])
            .label(gettext(
                "Your name and About text are visible to your contacts.",
            ))
            .build();

        let save = gtk::Button::builder()
            .label(gettext("Save"))
            .css_classes(["suggested-action", "pill"])
            .halign(gtk::Align::Center)
            .sensitive(false)
            .build();
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_top(12)
            .margin_bottom(24)
            .margin_start(18)
            .margin_end(18)
            .build();
        page.append(&avatar);
        page.append(&photo_buttons);
        page.append(&group);
        page.append(&hint);
        page.append(&save);
        page.append(&privacy_group(&init, on_setting));
        let scroller = gtk::ScrolledWindow::builder()
            .child(&adw::Clamp::builder().maximum_size(420).child(&page).build())
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let header = adw::HeaderBar::new();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&scroller));
        let dialog = adw::Dialog::builder()
            .title(gettext("Profile"))
            .content_width(440)
            .content_height(520)
            .child(&toolbar)
            .default_widget(&save)
            .build();

        let refresh: Rc<dyn Fn()> = {
            let (state, name, about, save) =
                (state.clone(), name.clone(), about.clone(), save.clone());
            Rc::new(move || {
                let state = state.borrow();
                let (name_text, about_text) = (name.text().to_string(), about.text().to_string());
                let invalid = problem(&name_text, &about_text);
                name.set_tooltip_text(invalid.filter(|_| name_text.trim().is_empty()));
                for (row, bad) in [
                    (
                        &name,
                        name_text.trim().is_empty() || name_text.chars().count() > NAME_LIMIT,
                    ),
                    (&about, about_text.chars().count() > ABOUT_LIMIT),
                ] {
                    if bad {
                        row.add_css_class("error");
                    } else {
                        row.remove_css_class("error");
                    }
                }
                let changed = name_text.trim() != state.name
                    || (state.about_loaded && about_text != state.about)
                    || state.picture != PictureEdit::Keep;
                save.set_sensitive(changed && invalid.is_none());
            })
        };
        for row in [&name, &about] {
            let refresh = refresh.clone();
            row.connect_changed(move |_| refresh());
        }

        {
            let (state, avatar, remove, refresh, dialog) = (
                state.clone(),
                avatar.clone(),
                remove.clone(),
                refresh.clone(),
                dialog.clone(),
            );
            change.connect_clicked(move |_| {
                let images = gtk::FileFilter::new();
                images.set_name(Some(&gettext("Images")));
                images.add_mime_type("image/*");
                let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
                filters.append(&images);
                let chooser = gtk::FileDialog::builder()
                    .title(gettext("Choose a Profile Photo"))
                    .filters(&filters)
                    .build();
                let parent = dialog.root().and_downcast::<gtk::Window>();
                let (state, avatar, remove, refresh) = (
                    state.clone(),
                    avatar.clone(),
                    remove.clone(),
                    refresh.clone(),
                );
                chooser.open(
                    parent.as_ref(),
                    gtk::gio::Cancellable::NONE,
                    move |picked| {
                        let Some(path) = picked.ok().and_then(|file| file.path()) else {
                            return;
                        };
                        let Ok(texture) = gtk::gdk::Texture::from_filename(&path) else {
                            return;
                        };
                        avatar.set_custom_image(Some(&texture));
                        remove.set_visible(true);
                        state.borrow_mut().picture = PictureEdit::Set(path);
                        refresh();
                    },
                );
            });
        }
        {
            let (state, avatar, refresh) = (state.clone(), avatar.clone(), refresh.clone());
            remove.connect_clicked(move |button| {
                avatar.set_custom_image(gtk::gdk::Texture::NONE);
                button.set_visible(false);
                state.borrow_mut().picture = PictureEdit::Remove;
                refresh();
            });
        }

        {
            let (state, name, about, content) = (
                state.clone(),
                name.clone(),
                about.clone(),
                scroller.clone().upcast::<gtk::Widget>(),
            );
            save.connect_clicked(move |_| {
                let state = state.borrow();
                let (name, about) = (name.text().trim().to_owned(), about.text().to_string());
                content.set_sensitive(false);
                on_save(ProfileEdit {
                    name: (name != state.name).then_some(name),
                    about: (state.about_loaded && about != state.about).then_some(about),
                    picture: state.picture.clone(),
                });
            });
        }

        dialog.present(Some(parent));
        Self {
            own_id: init.own_id,
            dialog,
            about,
            content: scroller.upcast(),
            state,
            refresh,
        }
    }

    pub(super) fn is_for(&self, id: &str) -> bool {
        self.own_id == id
    }

    /// Fills in the current About text unless the user has already typed one.
    pub(super) fn set_about(&self, about: &str) {
        {
            let mut state = self.state.borrow_mut();
            if state.about_loaded {
                return;
            }
            state.about_loaded = true;
            state.about = about.to_owned();
        }
        if self.about.text().is_empty() {
            self.about.set_text(about);
        }
        (self.refresh)();
    }

    /// Closes on success; on failure shows the reason and lets the user retry.
    pub(super) fn saved(&self, error: Option<&str>) {
        match error {
            None => {
                self.dialog.close();
            }
            Some(error) => {
                self.content.set_sensitive(true);
                if let Some(window) = self.dialog.root().and_downcast::<gtk::Window>()
                    && let Some(overlay) = window.child().and_downcast::<adw::ToastOverlay>()
                {
                    overlay.add_toast(adw::Toast::builder().title(error).use_markup(false).build());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::problem;

    #[test]
    fn a_blank_or_oversized_profile_cannot_be_saved() {
        assert_eq!(problem("Ada", ""), None);
        assert!(problem("   ", "").is_some());
        assert!(problem(&"a".repeat(26), "").is_some());
        assert!(problem("Ada", &"a".repeat(140)).is_some());
        assert_eq!(problem(&"a".repeat(25), &"a".repeat(139)), None);
    }
}
