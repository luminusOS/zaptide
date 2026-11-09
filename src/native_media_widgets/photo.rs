use super::{
    video_stream::VideoStream,
    viewer::{PhotoItem, ViewerDetails},
    *,
};
use crate::i18n::gettext;

/// Largest side of a photo in the transcript, in logical pixels.
const PHOTO_EDGE: u32 = 300;

/// Photo size in the transcript: its aspect ratio within `PHOTO_EDGE`, and
/// not so thin that it vanishes. Unknown sizes show square.
pub(super) fn photo_size(width: Option<u32>, height: Option<u32>) -> (i32, i32) {
    let (Some(width), Some(height)) = (width.filter(|w| *w > 0), height.filter(|h| *h > 0)) else {
        return (PHOTO_EDGE as i32, PHOTO_EDGE as i32);
    };
    let scale = f64::from(PHOTO_EDGE) / f64::from(width.max(height));
    let side = |value: u32| ((f64::from(value) * scale).round() as i32).max(PHOTO_EDGE as i32 / 3);
    (side(width), side(height))
}

/// A rounded frame of `width` by `height` showing the message thumbnail,
/// with room for overlays such as play or download buttons.
fn media_frame(
    message: &Message,
    width: i32,
    height: i32,
    label: &str,
) -> (gtk::Picture, gtk::Overlay) {
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .can_shrink(true)
        .width_request(width)
        .height_request(height)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label(label)]);
    // A picture's natural size is its texture's, loaded at twice the frame
    // for dense screens; the clamp keeps the frame at its logical size.
    let clamp = adw::Clamp::builder()
        .maximum_size(width)
        .tightening_threshold(width)
        .child(&picture)
        .build();
    // Pictures do not clip their own drawing; a frame with hidden overflow
    // rounds the corners.
    let frame = gtk::Overlay::builder()
        .child(&clamp)
        .halign(gtk::Align::Start)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["zaptide-photo"])
        .build();
    if let Some(texture) = message
        .thumbnail
        .as_deref()
        .and_then(inline_thumbnail_texture)
    {
        picture.set_paintable(Some(&texture));
    }
    (picture, frame)
}

/// A photo in its own rounded frame: the small inline thumbnail at once,
/// the file itself once downloaded, and a full view on click.
pub(super) fn append_photo(
    parent: &gtk::Box,
    message: &Message,
    media: &crate::model::Media,
    token: &DecodeToken,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let size = photo_size(media.width, media.height);
    append_photo_sized(parent, message, size, token, on_action, None, true);
}

/// Tiles an album shows, in two columns of this side; a larger album shows a
/// "+N" count on its last tile instead of the rest.
const ALBUM_TILES: usize = 4;
const ALBUM_COLUMNS: i32 = 2;
const ALBUM_SIDE: i32 = 150;

/// Several photos sent together, as one grid of square tiles. Each tile keeps
/// the single photo's behaviour: thumbnail, download, and full view. Beyond
/// four photos the last tile counts the rest and offers one download for all.
pub fn build_album_widget(
    messages: &[Message],
    on_action: impl Fn(NativeMediaAction) + 'static,
) -> NativeMediaWidget {
    let mut decode_token = DecodeToken::default();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let grid = gtk::Grid::builder()
        .row_spacing(3)
        .column_spacing(3)
        .halign(gtk::Align::Start)
        .build();
    let on_action: std::rc::Rc<dyn Fn(NativeMediaAction)> = std::rc::Rc::new(on_action);
    let hidden = messages.len().saturating_sub(ALBUM_TILES);
    // A larger album has one download button for all of its photos.
    let downloads: Vec<NativeMediaAction> = messages
        .iter()
        .filter_map(|message| match attachment_action(message) {
            Some(action @ NativeMediaAction::Download { .. }) => Some(action),
            _ => None,
        })
        .collect();
    // The viewer steps through the photos already on disk.
    let items: std::rc::Rc<Vec<PhotoItem>> = std::rc::Rc::new(
        messages
            .iter()
            .filter_map(|message| match attachment_action(message) {
                Some(NativeMediaAction::Open(path)) => Some(PhotoItem {
                    path,
                    details: ViewerDetails::of(message),
                }),
                _ => None,
            })
            .collect(),
    );
    let mut opened = 0;
    for (index, message) in messages.iter().take(ALBUM_TILES).enumerate() {
        let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tile.set_widget_name(&message.id);
        tile.add_css_class("zaptide-album-tile");
        let tile_token = DecodeToken::default();
        let viewer =
            matches!(attachment_action(message), Some(NativeMediaAction::Open(_))).then(|| {
                opened += 1;
                (items.clone(), opened - 1)
            });
        append_photo_sized(
            &tile,
            message,
            (ALBUM_SIDE, ALBUM_SIDE),
            &tile_token,
            on_action.clone(),
            viewer,
            hidden == 0,
        );
        decode_token.adopt(tile_token);
        let last = hidden > 0 && index + 1 == ALBUM_TILES;
        let cell: gtk::Widget = if last {
            let overlay = gtk::Overlay::builder()
                .child(&tile)
                .overflow(gtk::Overflow::Hidden)
                .css_classes(["zaptide-photo"])
                .build();
            overlay.add_overlay(&album_more(hidden));
            overlay.upcast()
        } else {
            tile.upcast()
        };
        grid.attach(
            &cell,
            index as i32 % ALBUM_COLUMNS,
            index as i32 / ALBUM_COLUMNS,
            1,
            1,
        );
    }
    // One download for the whole album, centred where the four tiles meet.
    let album = gtk::Overlay::builder()
        .child(&grid)
        .halign(gtk::Align::Start)
        .build();
    if hidden > 0 && !downloads.is_empty() {
        album.add_overlay(&album_download(downloads, on_action));
    }
    root.append(&album);
    NativeMediaWidget {
        widget: root,
        decode_token,
    }
}

/// The scrim over an album's last tile, with how many photos are not shown
/// centred on it. Clicks pass through to the tile below.
fn album_more(hidden: usize) -> gtk::Box {
    let scrim = gtk::Box::builder()
        .can_target(false)
        .css_classes(["zaptide-album-more"])
        .build();
    scrim.append(
        &gtk::Label::builder()
            .label(format!("+{hidden}"))
            .hexpand(true)
            .vexpand(true)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .css_classes(["title-1"])
            .build(),
    );
    scrim
}

/// The one button, centred on the album, that downloads every photo still missing.
fn album_download(
    downloads: Vec<NativeMediaAction>,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) -> gtk::Button {
    let icon = gtk::Image::builder()
        .icon_name("folder-download-symbolic")
        .pixel_size(28)
        .build();
    let button = gtk::Button::builder()
        .child(&icon)
        .width_request(56)
        .height_request(56)
        .tooltip_text(format!("Download {} photos", downloads.len()))
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["osd", "circular"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&gettext(
        "Download all photos",
    ))]);
    button.connect_clicked(move |_| {
        for action in &downloads {
            on_action(action.clone());
        }
    });
    button
}

fn append_photo_sized(
    parent: &gtk::Box,
    message: &Message,
    (width, height): (i32, i32),
    token: &DecodeToken,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
    viewer: Option<(std::rc::Rc<Vec<PhotoItem>>, usize)>,
    download_button: bool,
) {
    let (picture, frame) = media_frame(message, width, height, "Photo");
    match attachment_action(message) {
        Some(NativeMediaAction::Open(path)) => {
            // Twice the frame, for high-density screens.
            load_photo(
                &picture,
                path.clone(),
                2 * width as u32,
                2 * height as u32,
                token,
            );
            // A button, so the full view opens from the keyboard too.
            let button = gtk::Button::builder()
                .child(&frame)
                .halign(gtk::Align::Start)
                .tooltip_text(gettext("View photo"))
                .css_classes(["flat", "zaptide-photo-button"])
                .build();
            button.update_property(&[gtk::accessible::Property::Label(&gettext("View photo"))]);
            let (items, start) = viewer.unwrap_or_else(|| {
                let item = PhotoItem {
                    path,
                    details: ViewerDetails::of(message),
                };
                (std::rc::Rc::new(vec![item]), 0)
            });
            button.connect_clicked(move |_| {
                on_action(NativeMediaAction::ViewPhotos {
                    paths: items.iter().map(|item| item.path.clone()).collect(),
                    start,
                });
            });
            parent.append(&button);
        }
        Some(action @ NativeMediaAction::Download { .. }) => {
            let button = gtk::Button::builder()
                .icon_name("folder-download-symbolic")
                .tooltip_text(gettext("Download photo"))
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .css_classes(["osd", "circular"])
                .build();
            button.connect_clicked(move |_| on_action(action.clone()));
            if download_button {
                frame.add_overlay(&button);
            }
            parent.append(&frame);
        }
        Some(
            NativeMediaAction::AnswerButton { .. }
            | NativeMediaAction::AnswerListRow { .. }
            | NativeMediaAction::CopyContactPhone(_)
            | NativeMediaAction::OpenContact(_)
            | NativeMediaAction::MessageContact { .. }
            | NativeMediaAction::ViewPhotos { .. }
            | NativeMediaAction::ViewVideo(_),
        )
        | None => {
            let spinner = adw::Spinner::builder()
                .width_request(32)
                .height_request(32)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .build();
            frame.add_overlay(&spinner);
            parent.append(&frame);
        }
    }
}

/// A video as a framed thumbnail with a play button and its length. GIFs
/// loop silently in place; other videos open in the viewer with sound.
pub(super) fn append_video(
    parent: &gtk::Box,
    message: &Message,
    media: &crate::model::Media,
    seconds: Option<u32>,
    gif: bool,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let (width, height) = photo_size(media.width, media.height);
    let (picture, frame) = media_frame(message, width, height, if gif { "GIF" } else { "Video" });
    let length = if gif {
        Some("GIF".to_owned())
    } else {
        seconds.map(crate::util::duration)
    };
    let action = attachment_action(message);
    let badge = match (&action, length) {
        (Some(NativeMediaAction::Download { .. }), Some(length)) => {
            Some(format!("{length} · {}", crate::util::bytes(media.size)))
        }
        (Some(NativeMediaAction::Download { .. }), None) => Some(crate::util::bytes(media.size)),
        (_, length) => length,
    };
    if let Some(badge) = badge {
        frame.add_overlay(
            &gtk::Label::builder()
                .label(badge)
                .halign(gtk::Align::Start)
                .valign(gtk::Align::End)
                .margin_start(8)
                .margin_bottom(8)
                .css_classes(["caption", "zaptide-media-badge"])
                .build(),
        );
    }
    let centered = |icon: &str, tooltip: &str| {
        gtk::Button::builder()
            .icon_name(icon)
            .tooltip_text(tooltip)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .css_classes(["osd", "circular", "zaptide-play"])
            .build()
    };
    match action {
        Some(NativeMediaAction::Open(path)) => {
            let projection = playback_projection(message);
            if projection == (PlaybackProjection::Native { looping: true }) {
                picture.connect_map(move |picture| {
                    let Ok(clip) = VideoStream::open(&path, 600, 600) else {
                        log::warn!("video file could not be opened");
                        return;
                    };
                    clip.set_loop(true);
                    clip.set_muted(true);
                    picture.set_paintable(Some(&clip));
                    clip.play();
                });
                let thumbnail = picture.paintable();
                // ListView caches unmapped rows, so dropping the row is not
                // enough to release its decoder and full-resolution buffers.
                picture.connect_unmap(move |picture| {
                    if let Some(clip) = picture.paintable().and_downcast::<VideoStream>() {
                        clip.close();
                    }
                    picture.set_paintable(thumbnail.as_ref());
                });
                parent.append(&frame);
                return;
            }
            // Only a cue: the whole frame is the button.
            let play = gtk::Image::builder()
                .icon_name("media-playback-start-symbolic")
                .pixel_size(24)
                .halign(gtk::Align::Center)
                .valign(gtk::Align::Center)
                .css_classes(["osd", "zaptide-play"])
                .build();
            frame.add_overlay(&play);
            let playable = matches!(projection, PlaybackProjection::Native { .. });
            let button = gtk::Button::builder()
                .child(&frame)
                .halign(gtk::Align::Start)
                .tooltip_text(if playable { "Play video" } else { "Open video" })
                .css_classes(["flat", "zaptide-photo-button"])
                .build();
            button.update_property(&[gtk::accessible::Property::Label(&gettext("Play video"))]);
            button.connect_clicked(move |_| {
                if playable {
                    on_action(NativeMediaAction::ViewVideo(path.clone()));
                } else {
                    on_action(NativeMediaAction::Open(path.clone()));
                }
            });
            parent.append(&button);
        }
        Some(action @ NativeMediaAction::Download { .. }) => {
            let button = centered("folder-download-symbolic", "Download video");
            button.connect_clicked(move |_| on_action(action.clone()));
            frame.add_overlay(&button);
            parent.append(&frame);
        }
        Some(
            NativeMediaAction::AnswerButton { .. }
            | NativeMediaAction::AnswerListRow { .. }
            | NativeMediaAction::CopyContactPhone(_)
            | NativeMediaAction::OpenContact(_)
            | NativeMediaAction::MessageContact { .. }
            | NativeMediaAction::ViewPhotos { .. }
            | NativeMediaAction::ViewVideo(_),
        )
        | None => {
            frame.add_overlay(
                &adw::Spinner::builder()
                    .width_request(32)
                    .height_request(32)
                    .halign(gtk::Align::Center)
                    .valign(gtk::Align::Center)
                    .build(),
            );
            parent.append(&frame);
        }
    }
}

/// Opens the file manager at `path` with the file selected, through the
/// portal inside Flatpak.
pub fn show_in_folder(widget: &impl IsA<gtk::Widget>, path: &std::path::Path) {
    let window = widget.as_ref().root().and_downcast::<gtk::Window>();
    gtk::FileLauncher::new(Some(&gtk::gio::File::for_path(path))).open_containing_folder(
        window.as_ref(),
        gtk::gio::Cancellable::NONE,
        |result| {
            if let Err(error) = result {
                log::warn!("could not show the attachment in its folder: {error}");
            }
        },
    );
}

fn show_in_folder_button(path: &std::path::Path) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text(gettext("Show in Folder"))
        .valign(gtk::Align::Center)
        .build();
    let path = path.to_path_buf();
    button.connect_clicked(move |button| show_in_folder(button, &path));
    button
}

/// A document as a file row: its type icon, name, size and pages, and the
/// actions its download state allows.
pub(super) fn document_card(
    message: &Message,
    media: &crate::model::Media,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) -> gtk::Box {
    let (file_name, detail) = match project_content(message).content {
        NativeMediaContent::Document { file_name, detail } => (file_name, detail),
        _ => ("Document".to_owned(), String::new()),
    };
    let card = gtk::Box::builder()
        .spacing(12)
        .css_classes(["card", "zaptide-media-card", "zaptide-document"])
        .build();
    let (content_type, _) = gtk::gio::content_type_guess(Some(file_name.as_str()), None);
    let icon = gtk::Image::builder()
        .gicon(&gtk::gio::content_type_get_icon(&content_type))
        .pixel_size(40)
        .valign(gtk::Align::Center)
        .build();
    card.append(&icon);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(
        &gtk::Label::builder()
            .label(&file_name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(32)
            .tooltip_text(&file_name)
            .css_classes(["heading"])
            .build(),
    );
    let kind = gtk::gio::content_type_get_description(&content_type);
    let detail = [kind.as_str(), detail.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    text.append(
        &gtk::Label::builder()
            .label(&detail)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "dim-label"])
            .build(),
    );
    card.append(&text);
    let actions = gtk::Box::builder().css_classes(["linked"]).build();
    match attachment_action(message) {
        Some(NativeMediaAction::Open(path)) => {
            let open = gtk::Button::builder()
                .icon_name("adw-external-link-symbolic")
                .tooltip_text(gettext("Open"))
                .valign(gtk::Align::Center)
                .build();
            let target = path.clone();
            open.connect_clicked(move |_| on_action(NativeMediaAction::Open(target.clone())));
            actions.append(&open);
            actions.append(&show_in_folder_button(&path));
        }
        Some(action @ NativeMediaAction::Download { .. }) => {
            let download = gtk::Button::builder()
                .icon_name("folder-download-symbolic")
                .tooltip_text(format!("Download ({})", crate::util::bytes(media.size)))
                .valign(gtk::Align::Center)
                .build();
            download.connect_clicked(move |_| on_action(action.clone()));
            actions.append(&download);
        }
        Some(
            NativeMediaAction::AnswerButton { .. }
            | NativeMediaAction::AnswerListRow { .. }
            | NativeMediaAction::CopyContactPhone(_)
            | NativeMediaAction::OpenContact(_)
            | NativeMediaAction::MessageContact { .. }
            | NativeMediaAction::ViewPhotos { .. }
            | NativeMediaAction::ViewVideo(_),
        )
        | None => actions.append(
            &adw::Spinner::builder()
                .width_request(24)
                .height_request(24)
                .valign(gtk::Align::Center)
                .tooltip_text(gettext("Downloading"))
                .build(),
        ),
    }
    card.append(&actions);
    card
}

/// How long glycin keeps a loader process after its last use.
///
/// glycin built for tokio never cancels the D-Bus task of a loader it retires,
/// so each retirement leaves the loader's socket open for good, and enough of
/// them exhaust the process's descriptors. Its default of 30 seconds retired
/// loaders all day; this keeps retirement rare, and still frees the memory of
/// an app left idle. Drop it once glycin cancels that task.
const LOADER_RETENTION: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

fn photo_pool() -> std::sync::Arc<glycin::Pool> {
    static POOL: std::sync::LazyLock<std::sync::Arc<glycin::Pool>> =
        std::sync::LazyLock::new(|| {
            glycin::Pool::new(glycin::PoolConfig::new().retention_time(LOADER_RETENTION))
        });
    POOL.clone()
}

async fn open_photo(path: &std::path::Path) -> Result<glycin::Image, glycin::Error> {
    let mut loader = glycin::Loader::new(gtk::gio::File::for_path(path));
    loader.accepted_memory_formats(glycin::MemoryFormatSelection::R8g8b8a8);
    loader.pool(photo_pool());
    // Left on Auto, glycin runs its file and D-Bus work on the GTK main context
    // because this thread has none of its own, and a large photo then stalls
    // the window while it loads.
    loader.main_context_selector(glycin::MainContextSelector::Managed);
    loader.load().await
}

fn photo_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::LazyLock<tokio::runtime::Runtime> = std::sync::LazyLock::new(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("zaptide-photo")
            .enable_all()
            .build()
            .expect("photo runtime")
    });
    &RUNTIME
}

/// Loads `path` through glycin's sandboxed loaders, scaled to fit
/// `width` by `height`, into `picture`, unless its row moved on first.
fn load_photo(
    picture: &gtk::Picture,
    path: std::path::PathBuf,
    width: u32,
    height: u32,
    token: &DecodeToken,
) {
    let ticket = token.issue();
    let picture = glib::SendWeakRef::from(picture.downgrade());
    let main_context = glib::MainContext::default();
    let current = ticket.clone();
    photo_runtime().spawn(async move {
        let texture = decode_photo_file(&path, width, height, &current).await;
        main_context.invoke(move || {
            if let (true, Some(picture), Some(texture)) =
                (ticket.is_current(), picture.upgrade(), texture)
            {
                picture.set_paintable(Some(&texture));
            }
        });
    });
}

/// Like `load_photo`, for a caller that wants the texture itself: `done`
/// runs on the main thread with it, unless a newer decode of `token` or a
/// failure got there first.
pub(super) fn decode_photo(
    path: std::path::PathBuf,
    width: u32,
    height: u32,
    token: &DecodeToken,
    done: impl FnOnce(gdk::Texture) + 'static,
) {
    let ticket = token.issue();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let current = ticket.clone();
    photo_runtime().spawn(async move {
        let texture = decode_photo_file(&path, width, height, &current).await;
        let _ = sender.send(texture);
    });
    glib::spawn_future_local(async move {
        if let Ok(Some(texture)) = receiver.await
            && ticket.is_current()
        {
            done(texture);
        }
    });
}

async fn decode_photo_file(
    path: &std::path::Path,
    width: u32,
    height: u32,
    current: &crate::native_media::DecodeTicket,
) -> Option<gdk::Texture> {
    // Rows recycled while scrolling queue loads; skip the stale ones.
    if !current.is_current() {
        return None;
    }
    let mut image = open_photo(path).await.inspect_err(photo_error).ok()?;
    if !current.is_current() {
        return None;
    }
    let frame = image
        .specific_frame(glycin::FrameRequest::new().scale(width, height))
        .await
        .inspect_err(photo_error)
        .ok()?;
    Some(fit_frame(&frame, width, height))
}

/// Loaders may ignore the requested scale and return the full image, tens
/// of megabytes for a phone photo; shrink it to fit `width` by `height`.
fn fit_frame(frame: &glycin::Frame, width: u32, height: u32) -> gdk::Texture {
    let (frame_width, frame_height) = (frame.width(), frame.height());
    let fits = frame_width <= width && frame_height <= height;
    let rgba = frame.memory_format() == glycin::MemoryFormat::R8g8b8a8;
    let pixels = (!fits && rgba)
        .then(|| {
            let stride = frame.stride() as usize;
            let row = frame_width as usize * 4;
            let packed = frame
                .buf_slice()
                .chunks(stride)
                .take(frame_height as usize)
                .flat_map(|line| line.get(..row).unwrap_or_default().iter().copied())
                .collect::<Vec<u8>>();
            image::RgbaImage::from_raw(frame_width, frame_height, packed)
        })
        .flatten();
    let Some(pixels) = pixels else {
        return frame.texture();
    };
    let scale = f64::min(
        f64::from(width) / f64::from(frame_width),
        f64::from(height) / f64::from(frame_height),
    );
    let fitted = image::imageops::thumbnail(
        &pixels,
        ((f64::from(frame_width) * scale).round() as u32).max(1),
        ((f64::from(frame_height) * scale).round() as u32).max(1),
    );
    let (fitted_width, fitted_height) = fitted.dimensions();
    gdk::MemoryTexture::new(
        fitted_width as i32,
        fitted_height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(fitted.into_raw()),
        fitted_width as usize * 4,
    )
    .upcast()
}

/// Logs the first failure only: without glycin's loaders or bubblewrap,
/// every photo fails the same way and keeps its thumbnail.
fn photo_error(error: &impl std::fmt::Display) {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::warn!("photo could not be loaded: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_sockets() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .flatten()
            .filter(|entry| {
                std::fs::read_link(entry.path())
                    .is_ok_and(|target| target.to_string_lossy().starts_with("socket:"))
            })
            .count()
    }

    /// Needs glycin's loaders and bubblewrap, and waits out glycin's default
    /// retention time three times, so it runs only on request:
    /// `cargo test --lib loaders_do_not_leak -- --ignored`.
    #[test]
    #[ignore = "needs glycin loaders and bubblewrap; takes about two minutes"]
    fn loaders_do_not_leak_sockets_while_the_app_sits_idle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.png");
        image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
            .save(&path)
            .unwrap();
        let load = || {
            photo_runtime().block_on(async {
                let mut image = open_photo(&path).await.expect("loader starts");
                image
                    .specific_frame(glycin::FrameRequest::new().scale(8, 8))
                    .await
                    .expect("frame decodes");
            })
        };
        load();
        std::thread::sleep(std::time::Duration::from_secs(40));
        let settled = open_sockets();
        for _ in 0..2 {
            load();
            std::thread::sleep(std::time::Duration::from_secs(40));
        }
        assert_eq!(open_sockets(), settled);
    }
}
