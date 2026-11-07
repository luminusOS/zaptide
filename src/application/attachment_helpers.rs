use super::*;

impl NativeApplication {
    pub(super) fn selected_attachment(
        &self,
    ) -> Option<crate::native_attachments::AttachmentPresentation> {
        self.selected_message()
            .and_then(crate::native_attachments::project)
    }

    pub(super) fn attachment_action_available(&self) -> bool {
        self.selected_attachment().is_some_and(|attachment| {
            attachment.control != crate::native_attachments::AttachmentControl::Downloading
        })
    }

    pub(super) fn activate_attachment(&mut self, id: &str) {
        let Some(action) = self
            .open_chat
            .message_snapshots
            .get(id)
            .and_then(crate::native_attachments::project)
            .and_then(|attachment| {
                attachment.action(crate::native_attachments::AttachmentIntent::Activate)
            })
        else {
            return;
        };
        match action {
            crate::model::Action::Download { chat, message } => {
                if let Some(media) = self
                    .open_chat
                    .message_snapshots
                    .get_mut(&message)
                    .and_then(|message| message.content.media_mut())
                {
                    media.state = crate::model::MediaState::Downloading;
                }
                if let Some(backend) = &self.backend {
                    backend.send(crate::backend::Command::Download {
                        chat,
                        message: message.clone(),
                    });
                    self.status = gettext("Downloading attachment");
                }
                self.refresh_message_row(&message);
            }
            crate::model::Action::OpenFile(path) => {
                let content = self
                    .open_chat
                    .message_snapshots
                    .get(id)
                    .map(|message| &message.content);
                let declared = content
                    .and_then(|content| content.media())
                    .map(|media| media.mime.as_str())
                    .unwrap_or_default();
                if document_safety::path_needs_confirmation(&path, declared) {
                    let cached_name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let sender_name = match content {
                        Some(crate::model::Content::Document { file_name, .. })
                            if !file_name.is_empty() =>
                        {
                            file_name.clone()
                        }
                        _ => cached_name,
                    };
                    document_safety::confirm_and_open(&self.window, &path, &sender_name);
                    return;
                }
                let file = gtk::gio::File::for_path(path);
                if gtk::gio::AppInfo::launch_default_for_uri(
                    &file.uri(),
                    None::<&gtk::gio::AppLaunchContext>,
                )
                .is_err()
                {
                    self.status = gettext("Could not open attachment");
                }
            }
            _ => {}
        }
    }

    pub(super) fn paste_clipboard_image(&mut self, sender: &ComponentSender<Self>) {
        let Some(chat) = self
            .open_chat
            .active_chat
            .clone()
            .filter(|_| self.can_attach())
        else {
            return;
        };
        if !self.portal_requests.borrow().is_empty() {
            self.status = gettext("Finish the active portal action first");
            return;
        }
        let Some(display) = gtk::gdk::Display::default() else {
            self.status = gettext("Clipboard is unavailable");
            return;
        };
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        let request = self
            .portals
            .read_clipboard_image(&display.clipboard(), move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                let pixels = result
                    .map_err(|_| "Clipboard image is unavailable".to_owned())
                    .and_then(|texture| {
                        let Some(texture) = texture else {
                            return Err("Clipboard does not contain an image".into());
                        };
                        let width = texture.width() as u32;
                        let height = texture.height() as u32;
                        let Some(length) = (width as usize)
                            .checked_mul(height as usize)
                            .and_then(|pixels| pixels.checked_mul(4))
                        else {
                            return Err("Clipboard image is too large".into());
                        };
                        if width == 0
                            || height == 0
                            || u64::from(width) * u64::from(height) > 16_777_216
                        {
                            return Err("Clipboard image is too large".into());
                        }
                        let mut rgba = vec![0; length];
                        texture.download(&mut rgba, width as usize * 4);
                        convert_premultiplied_bgra_to_rgba(&mut rgba);
                        Ok(ClipboardPixels {
                            width,
                            height,
                            preview: gtk::gdk::MemoryTexture::new(
                                width as i32,
                                height as i32,
                                gtk::gdk::MemoryFormat::R8g8b8a8,
                                &gtk::glib::Bytes::from_owned(rgba.clone()),
                                width as usize * 4,
                            )
                            .into(),
                            rgba,
                        })
                    });
                match pixels {
                    Ok(pixels) => input.input(Input::ClipboardImageReady { chat, pixels }),
                    Err(error) => input.input(Input::PortalActionFinished(error)),
                }
            });
        if let Some(id) = request {
            request_id.set(Some(id));
            self.portal_requests.borrow_mut().insert(id);
            self.status = gettext("Reading clipboard image");
        } else {
            self.status = gettext("Could not read clipboard image");
        }
    }

    pub(super) fn stage_clipboard_image(&mut self, chat: String, pixels: ClipboardPixels) {
        if self.open_chat.active_chat.as_deref() != Some(&chat) || !self.can_attach() {
            self.status = gettext("Choose an available conversation before pasting an image");
            return;
        }
        if self.pending_send.is_some()
            || self.audio.voice_send_pending
            || self
                .pending_attachments
                .get(&chat)
                .is_some_and(|paths| !paths.is_empty())
            || self
                .pending_clipboard_images
                .keys()
                .any(|staged_chat| staged_chat != &chat)
        {
            self.status = gettext("Send or clear the staged attachment before pasting an image");
            return;
        }
        self.pending_clipboard_images.insert(chat, pixels);
        self.status = gettext("Clipboard image ready");
    }

    pub(super) fn open_selected_uri(&mut self, sender: &ComponentSender<Self>) {
        let Some(uri) = self
            .selected_message()
            .and_then(|message| match &message.content {
                crate::model::Content::Text {
                    preview: Some(preview),
                    ..
                } => Some(preview.url.clone()),
                _ => None,
            })
        else {
            return;
        };
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let callback_id = request_id.clone();
        match self.portals.open_uri(
            crate::native_portals::UserAction::for_explicit_user_action(),
            &uri,
            move |result| {
                if let Some(id) = callback_id.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::PortalUriFinished(result.is_ok()));
            },
        ) {
            Ok(id) => {
                request_id.set(Some(id));
                self.portal_requests.borrow_mut().insert(id);
                self.status = gettext("Opening link");
            }
            Err(_) => self.status = gettext("Link is not supported"),
        }
    }

    pub(super) fn save_selected_attachment(&mut self, sender: &ComponentSender<Self>) {
        let Some(path) = self
            .selected_message()
            .and_then(|message| message.content.media())
            .and_then(|media| media.path.clone())
        else {
            return;
        };
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "attachment".into());
        let input = sender.clone();
        let requests = self.portal_requests.clone();
        let callback_id = std::rc::Rc::new(std::cell::Cell::new(None));
        let id_slot = callback_id.clone();
        let source = gtk::gio::File::for_path(path);
        let request = self.portals.save_copy(
            Some(&self.window),
            &source,
            "Save attachment",
            &name,
            move |result| {
                if let Some(id) = id_slot.get() {
                    requests.borrow_mut().remove(&id);
                }
                input.input(Input::SaveAttachmentFinished(result.is_ok()));
            },
        );
        if let Some(id) = request {
            callback_id.set(Some(id));
            self.portal_requests.borrow_mut().insert(id);
            self.status = gettext("Choose save location");
        } else {
            self.status = gettext("Could not open save dialog");
        }
    }

    /// File names of the files staged for the open chat.
    pub(super) fn pending_attachment_names(&self) -> Vec<String> {
        self.open_chat
            .active_chat
            .as_ref()
            .and_then(|chat| self.pending_attachments.get(chat))
            .into_iter()
            .flatten()
            .map(|path| {
                path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }

    pub(super) fn pending_attachment_count(&self) -> usize {
        let files = self
            .open_chat
            .active_chat
            .as_ref()
            .and_then(|chat| self.pending_attachments.get(chat))
            .map_or(0, Vec::len);
        let image = self
            .open_chat
            .active_chat
            .as_ref()
            .is_some_and(|chat| self.pending_clipboard_images.contains_key(chat));
        files + usize::from(image)
    }
}
