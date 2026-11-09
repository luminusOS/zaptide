use super::*;

thread_local! {
    static COPY_GENERATION: DecodeToken = DecodeToken::default();
}

pub(super) struct ImageCopy {
    clipboard: gdk::Clipboard,
    changed: Option<glib::SignalHandlerId>,
    ticket: crate::native_media::DecodeTicket,
}

impl ImageCopy {
    pub(super) fn new(clipboard: &gdk::Clipboard) -> Self {
        let generation = COPY_GENERATION.with(Clone::clone);
        let ticket = generation.issue();
        let changed = clipboard.connect_changed(move |_| generation.cancel());
        Self {
            clipboard: clipboard.clone(),
            changed: Some(changed),
            ticket,
        }
    }

    pub(super) fn finish(self, texture: &gdk::Texture) {
        if self.ticket.is_current() {
            self.clipboard.set_texture(texture);
        }
    }
}

impl Drop for ImageCopy {
    fn drop(&mut self) {
        if let Some(changed) = self.changed.take() {
            self.clipboard.disconnect(changed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a display"]
    fn stale_image_copies_never_replace_newer_images_or_text() {
        gtk::init().unwrap();
        let clipboard = gdk::Display::default().unwrap().clipboard();
        let texture = |value| -> gdk::Texture {
            gdk::MemoryTexture::new(
                1,
                1,
                gdk::MemoryFormat::R8g8b8a8,
                &glib::Bytes::from_owned(vec![value, 0, 0, 255]),
                4,
            )
            .upcast()
        };
        let older = ImageCopy::new(&clipboard);
        let newer = ImageCopy::new(&clipboard);
        newer.finish(&texture(20));
        let current = clipboard.content();
        older.finish(&texture(10));
        assert_eq!(clipboard.content(), current);

        let pending = ImageCopy::new(&clipboard);
        clipboard.set_text("newer synthetic text");
        let current = clipboard.content();
        pending.finish(&texture(30));
        assert_eq!(clipboard.content(), current);

        let failed = ImageCopy::new(&clipboard);
        drop(failed);
        let next = ImageCopy::new(&clipboard);
        assert!(next.ticket.is_current());
        next.finish(&texture(40));
        assert_ne!(clipboard.content(), current);
    }
}
