use super::*;

pub(super) fn attach_dropped(
    application: &mut NativeApplication,
    paths: Vec<std::path::PathBuf>,
    sender: &ComponentSender<NativeApplication>,
) {
    let Some(chat) = application
        .active_chat
        .clone()
        .filter(|_| application.can_attach())
    else {
        application.status = "Choose a writable chat before attaching files".into();
        return;
    };
    if paths.is_empty() {
        return;
    }
    if application.pending_clipboard_images.contains_key(&chat) {
        application.status = "Clear the staged clipboard image before adding files".into();
        return;
    }
    application
        .composer
        .stage_attachment_caption(&chat, application.draft.clone());
    for path in &paths {
        application
            .document_attachments
            .remove(&(chat.clone(), path.clone()));
    }
    application
        .pending_attachments
        .entry(chat)
        .or_default()
        .extend(paths);
    application.status = attachment_summary(
        &application.pending_attachment_names(),
        application.pending_attachment_count(),
    );
    application.show_attachment_preview(sender);
}

pub(super) fn pick_attachments(
    application: &mut NativeApplication,
    gallery: bool,
    sender: &ComponentSender<NativeApplication>,
) {
    let Some(chat) = application
        .active_chat
        .clone()
        .filter(|_| application.can_attach())
    else {
        return;
    };
    if !application.portal_requests.borrow().is_empty() {
        application.status = "Finish the active portal action first".into();
        return;
    }
    if application.pending_clipboard_images.contains_key(&chat) {
        application.status = "Clear the staged clipboard image before adding files".into();
        return;
    }
    let input = sender.clone();
    let requests = application.portal_requests.clone();
    let request_id = std::rc::Rc::new(std::cell::Cell::new(None));
    let callback_request_id = request_id.clone();
    let (title, filter) = if gallery {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Photos and videos"));
        filter.add_mime_type("image/*");
        filter.add_mime_type("video/*");
        ("Gallery", Some(filter))
    } else {
        ("Send files as documents", None)
    };
    let request = application.portals.open_files(
        Some(&application.window),
        title,
        filter.as_ref(),
        move |result| {
            if let Some(id) = callback_request_id.get() {
                requests.borrow_mut().remove(&id);
            }
            let paths = result
                .unwrap_or_default()
                .into_iter()
                .filter_map(|file| file.path())
                .collect();
            input.input(Input::AttachmentsPicked {
                chat,
                paths,
                documents: !gallery,
            });
        },
    );
    if let Some(id) = request {
        request_id.set(Some(id));
        application.portal_requests.borrow_mut().insert(id);
    }
}

pub(super) fn attachments_picked(
    application: &mut NativeApplication,
    chat: String,
    paths: Vec<std::path::PathBuf>,
    documents: bool,
    sender: &ComponentSender<NativeApplication>,
) {
    if paths.is_empty() {
        return;
    }
    if application.pending_clipboard_images.contains_key(&chat) {
        application.status = "Clear the staged clipboard image before adding files".into();
        return;
    }
    application
        .composer
        .stage_attachment_caption(&chat, application.composer.draft(&chat).to_owned());
    for path in &paths {
        let key = (chat.clone(), path.clone());
        if documents {
            application.document_attachments.insert(key);
        } else {
            application.document_attachments.remove(&key);
        }
    }
    application
        .pending_attachments
        .entry(chat)
        .or_default()
        .extend(paths);
    application.show_attachment_preview(sender);
}

pub(super) fn clear_attachments(application: &mut NativeApplication) {
    if let Some(chat) = &application.active_chat {
        application.pending_attachments.remove(chat);
        application.pending_clipboard_images.remove(chat);
    }
}
