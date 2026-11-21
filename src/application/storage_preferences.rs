//! Wiring for the Storage preferences' download-location controls. The
//! preferences dialog itself stays free of portal and runtime-path concerns
//! (see `native_preferences`'s module doc); picking a folder needs both, so
//! it lives here instead.

use super::*;

impl NativeApplication {
    /// Opens a native folder picker for the download location. The result
    /// comes back asynchronously as `Input::DownloadDirectoryChosen`.
    pub(super) fn choose_download_directory(&mut self, sender: &ComponentSender<Self>) {
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        let title = gettext("Choose a Download Folder");
        let picked = self
            .portals
            .select_folder(Some(&self.window), &title, move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                // A cancelled or failed pick (`Ok(None)` or `Err`) leaves the
                // current setting untouched; only a chosen folder is applied.
                if let Ok(Some(file)) = result
                    && let Some(path) = file.path()
                {
                    input.input(Input::DownloadDirectoryChosen(Some(path)));
                }
            });
        match picked {
            Some(id) => {
                request_id.set(Some(id));
                self.portal_requests.borrow_mut().insert(id);
            }
            None => {
                self.toast(&gettext("Could not open the folder picker"));
            }
        }
    }

    /// Applies a folder chosen through the picker, or `None` to restore the
    /// default download location, through the same validated path every
    /// other preference edit takes so it is saved and synced consistently.
    pub(super) fn download_directory_chosen(&mut self, path: Option<std::path::PathBuf>) {
        let Some(dialog) = self.preferences.as_mut() else {
            // The preferences dialog was closed before the pick resolved;
            // nothing to show or save it against.
            return;
        };
        let as_string = path.map(|path| path.display().to_string());
        dialog.apply_download_directory_choice(as_string);
    }
}
