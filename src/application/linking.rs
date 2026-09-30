use super::NativeApplication;
use crate::backend::LinkStatus;
use relm4::gtk;

pub(super) fn link_page(link: &LinkStatus) -> (String, String) {
    match link {
        LinkStatus::Starting => (
            "Starting ZapTide".into(),
            "Preparing WhatsApp connection.".into(),
        ),
        // The code itself is shown large below the text, with a copy button.
        LinkStatus::Unlinked {
            pair_code: Some(_), ..
        } => (
            "Enter code on your phone".into(),
            "In WhatsApp on your phone, open Linked devices, tap Link a device, then \
             Link with phone number instead, and enter this code."
                .into(),
        ),
        LinkStatus::Unlinked { pairing_phone, .. } if pairing_phone.is_some() => (
            "Requesting pairing code".into(),
            "Waiting for WhatsApp to provide a pairing code.".into(),
        ),
        LinkStatus::Unlinked { qr: Some(_), .. } => (
            "Link this computer".into(),
            "Open WhatsApp, choose Linked devices, then scan the QR code.".into(),
        ),
        LinkStatus::Unlinked { .. } => (
            "Link this computer".into(),
            "Waiting for WhatsApp to provide a QR code.".into(),
        ),
        LinkStatus::Connecting => ("Connecting".into(), "Completing WhatsApp linking.".into()),
        LinkStatus::Connected => ("Connected".into(), "Loading your chats.".into()),
        LinkStatus::Disconnected { .. } => (
            "Reconnecting".into(),
            "Connection lost. ZapTide is reconnecting automatically.".into(),
        ),
        LinkStatus::LoggedOut => (
            "Phone unlinked this computer".into(),
            "Requesting a new WhatsApp link code.".into(),
        ),
        LinkStatus::Failed(_) => (
            "ZapTide needs attention".into(),
            "WhatsApp connection could not start. Check the desktop log for details.".into(),
        ),
    }
}

pub(super) fn qr_texture(qr: &str) -> Option<gtk::gdk::Texture> {
    const QUIET: usize = 4;
    // GtkPicture shows the texture at its pixel size; keep it near 264 px.
    const SIDE: usize = 264;
    let code = qrcode::QrCode::new(qr.as_bytes()).ok()?;
    let width = code.width();
    let module = (SIDE / (width + 2 * QUIET)).max(2);
    let side = (width + 2 * QUIET) * module;
    let mut image = image::RgbaImage::from_pixel(side as u32, side as u32, image::Rgba([255; 4]));
    for (index, color) in code.to_colors().into_iter().enumerate() {
        if color != qrcode::Color::Dark {
            continue;
        }
        let x0 = (index % width + QUIET) * module;
        let y0 = (index / width + QUIET) * module;
        for y in y0..y0 + module {
            for x in x0..x0 + module {
                image.put_pixel(x as u32, y as u32, image::Rgba([0, 0, 0, 255]));
            }
        }
    }
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut encoded, image::ImageFormat::Png)
        .ok()?;
    gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_owned(encoded.into_inner())).ok()
}

impl NativeApplication {
    pub(super) fn is_linked(&self) -> bool {
        matches!(
            self.link,
            LinkStatus::Connected | LinkStatus::Connecting | LinkStatus::Disconnected { .. }
        ) || (!self.chat_snapshots.is_empty() && !matches!(self.link, LinkStatus::LoggedOut))
    }

    pub(super) fn pair_code(&self) -> Option<&str> {
        match &self.link {
            LinkStatus::Unlinked {
                pair_code: Some(code),
                ..
            } => Some(code),
            _ => None,
        }
    }

    pub(super) fn pairing_requested(&self) -> bool {
        matches!(
            self.link,
            LinkStatus::Unlinked {
                pairing_phone: Some(_),
                ..
            }
        )
    }

    /// The link page is waiting on WhatsApp rather than on the user.
    pub(super) fn link_busy(&self) -> bool {
        match &self.link {
            LinkStatus::Starting | LinkStatus::Connecting | LinkStatus::Connected => true,
            LinkStatus::Unlinked { qr, pair_code, .. } => {
                pair_code.is_none()
                    && (self.pairing_requested() || (qr.is_none() && !self.phone_linking))
            }
            _ => false,
        }
    }
}
