//! Edits to this account's own profile: name, About text, and picture.

use super::*;
use crate::backend::{PictureEdit, ProfileEdit};

/// WhatsApp profile pictures are square JPEGs of this many pixels.
const PICTURE_SIZE: u32 = 640;

impl Worker {
    pub(super) fn edit_profile(&mut self, edit: ProfileEdit) {
        let Some(client) = self.client.clone().filter(|client| client.is_connected()) else {
            self.emit(Event::ProfileSaved {
                error: Some("Not connected to WhatsApp".into()),
            });
            return;
        };
        let commands = self.commands.clone();
        let session_generation = self.session_generation;
        tokio::spawn(async move {
            let result = apply(&client, &edit).await;
            let _ = commands.send(Command::ProfileSaved {
                session_generation,
                edit,
                result,
            });
        });
    }

    pub(super) fn profile_saved(
        &mut self,
        session_generation: u64,
        edit: ProfileEdit,
        result: Result<(), String>,
    ) {
        if session_generation != self.session_generation {
            return;
        }
        if result.is_ok() {
            if let Some(name) = edit.name {
                let _ = self.archive.set_meta("me_name", &name);
                self.me_name = Some(name);
                self.emit_profile();
            }
            if let Some(about) = edit.about {
                let _ = self.archive.set_meta("me_about", &about);
                self.me_about = Some(about).filter(|about| !about.is_empty());
            }
            if edit.picture != PictureEdit::Keep {
                self.refresh_own_avatar();
            }
        }
        self.emit(Event::ProfileSaved {
            error: result.err(),
        });
    }

    /// Drops the cached picture so the next fetch shows the new one.
    fn refresh_own_avatar(&mut self) {
        let Some(me) = self.me_pn.clone() else {
            return;
        };
        for full in [false, true] {
            let _ = std::fs::remove_file(self.avatar_file(&me, full));
        }
        self.fetch_avatar(me.clone(), false);
        self.fetch_avatar(me, true);
    }
}

async fn apply(client: &Client, edit: &ProfileEdit) -> Result<(), String> {
    let profile = client.profile();
    if let Some(name) = &edit.name {
        profile
            .set_push_name(name)
            .await
            .map_err(|_| "Could not save the name")?;
    }
    if let Some(about) = &edit.about {
        profile
            .set_status_text(about)
            .await
            .map_err(|_| "Could not save the About text")?;
    }
    match &edit.picture {
        PictureEdit::Keep => {}
        PictureEdit::Remove => {
            profile
                .remove_profile_picture()
                .await
                .map_err(|_| "Could not remove the picture")?;
        }
        PictureEdit::Set(path) => {
            let path = path.clone();
            let jpeg = tokio::task::spawn_blocking(move || square_jpeg(&path))
                .await
                .map_err(|_| "Could not read the picture")??;
            profile
                .set_profile_picture(jpeg)
                .await
                .map_err(|_| "Could not save the picture")?;
        }
    }
    Ok(())
}

/// The picture cropped to a centred square and encoded as JPEG.
fn square_jpeg(path: &std::path::Path) -> Result<Vec<u8>, &'static str> {
    let image = image::open(path)
        .map_err(|_| "This file is not a picture ZapTide can read")?
        .resize_to_fill(
            PICTURE_SIZE,
            PICTURE_SIZE,
            image::imageops::FilterType::Lanczos3,
        )
        .to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&image)
        .map_err(|_| "Could not prepare the picture")?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    #[test]
    fn pictures_are_cropped_to_a_square_jpeg() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("wide.png");
        image::RgbaImage::from_pixel(200, 100, image::Rgba([10, 120, 200, 255]))
            .save(&source)
            .unwrap();
        let jpeg = super::square_jpeg(&source).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!(
            (decoded.width(), decoded.height()),
            (super::PICTURE_SIZE, super::PICTURE_SIZE)
        );
    }

    #[test]
    fn a_file_that_is_not_a_picture_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("notes.txt");
        std::fs::write(&source, "hello").unwrap();
        assert!(super::square_jpeg(&source).is_err());
    }
}
