use super::*;

impl NativeApplication {
    pub(super) fn themes_directory(&self) -> std::path::PathBuf {
        self.settings_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("themes")
    }

    pub(super) fn theme_choice_filenames(&self) -> Vec<String> {
        self.theme_catalog
            .picker_themes()
            .map(|theme| theme.filename.clone())
            .collect()
    }

    pub(super) fn sync_custom_theme_selection(&mut self) {
        if let Some(filename) = self.settings.custom_theme.clone() {
            if let Some(theme) = self.theme_catalog.find(&filename).cloned() {
                self.settings.custom_theme_cache = Some(theme);
            } else {
                self.theme_catalog
                    .start(self.themes_directory(), Some(filename), &self.notifier);
            }
        } else {
            self.settings.custom_theme_cache = None;
        }
        self.apply_runtime_settings();
    }

    pub(super) fn poll_theme_catalog(&mut self) {
        if self.theme_catalog.needs_reload() {
            self.theme_catalog.start(
                self.themes_directory(),
                self.settings.custom_theme.clone(),
                &self.notifier,
            );
        }
        if !self.theme_catalog.poll() {
            return;
        }

        let previous_palette = self.settings.cached_palette();
        if let Some(filename) = self.settings.custom_theme.as_deref()
            && let Some(theme) = self.theme_catalog.find(filename).cloned()
        {
            self.settings.custom_theme_cache = Some(theme);
        }

        if self.settings.cached_palette() != previous_palette {
            self.apply_runtime_settings();
            if self.save_settings().is_err() {
                self.status = gettext("Could not save theme preference");
            }
        }
        let choices = self.theme_choice_filenames();
        let selected = self.settings.custom_theme.clone();
        if let Some(dialog) = self.preferences.as_mut() {
            dialog.set_custom_theme_choices(&choices, selected.as_deref());
        }
    }

    pub(super) fn install_zoom_provider(&self) {
        if let Some(display) = gtk::gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &self.zoom_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            gtk::style_context_add_provider_for_display(
                &display,
                &self.custom_theme_provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
            );
        }
    }

    pub(super) fn apply_runtime_settings(&mut self) {
        apply_theme(&self.settings, &self.custom_theme_provider);
        self.zoom_provider.load_from_string(&format!(
            "window {{ font-size: {:.0}%; }}",
            100.0 * self.settings.zoom
        ));
        self.enter_sends.set(self.settings.enter_sends);
        self.audio.media.set_speed(self.settings.voice_speed);
        self.sync_translucent_sidebar();
    }

    pub(super) fn sync_translucent_sidebar(&self) {
        let collapsed = self
            .split_view
            .as_ref()
            .is_some_and(adw::NavigationSplitView::is_collapsed);
        let active = translucent_sidebar_active(&self.settings, self.is_linked(), collapsed);
        if active {
            self.window.add_css_class("translucent-sidebar");
        } else {
            self.window.remove_css_class("translucent-sidebar");
        }
    }

    pub(super) fn prepare_themes_folder(&mut self, sender: &ComponentSender<Self>) {
        let Some(config_dir) = self.settings_path.parent() else {
            self.status = gettext("Could not open themes folder");
            self.toast(&gettext("Could not open themes folder"));
            return;
        };
        let folder = config_dir.join("themes");
        let sender = sender.clone();
        std::thread::spawn(move || {
            let success = std::fs::create_dir_all(folder).is_ok();
            sender.input(Input::ThemesFolderPrepared(success));
        });
        self.status = gettext("Preparing themes folder");
    }

    pub(super) fn launch_themes_folder(&mut self, sender: &ComponentSender<Self>) {
        let Some(config_dir) = self.settings_path.parent() else {
            self.status = gettext("Could not open themes folder");
            self.toast(&gettext("Could not open themes folder"));
            return;
        };
        let folder = config_dir.join("themes");
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        match self.portals.open_folder(
            crate::native_portals::UserAction::for_explicit_user_action(),
            &folder,
            move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::ThemesFolderFinished(result.is_ok()));
            },
        ) {
            Ok(id) => {
                request_id.set(Some(id));
                self.portal_requests.borrow_mut().insert(id);
                self.status = gettext("Opening themes folder");
            }
            Err(_) => {
                self.status = gettext("Could not open themes folder");
                self.toast(&gettext("Could not open themes folder"));
            }
        }
    }
}
