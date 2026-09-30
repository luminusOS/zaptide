use super::*;

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
        .and_then(|bytes| gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok())
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
    button.update_property(&[gtk::accessible::Property::Label("Download all photos")]);
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
                .tooltip_text("View photo")
                .css_classes(["flat", "zaptide-photo-button"])
                .build();
            button.update_property(&[gtk::accessible::Property::Label("View photo")]);
            let (items, start) = viewer.unwrap_or_else(|| {
                let item = PhotoItem {
                    path,
                    details: ViewerDetails::of(message),
                };
                (std::rc::Rc::new(vec![item]), 0)
            });
            button.connect_clicked(move |button| {
                show_photo(button, items.clone(), start, on_action.clone());
            });
            parent.append(&button);
        }
        Some(action @ NativeMediaAction::Download { .. }) => {
            let button = gtk::Button::builder()
                .icon_name("folder-download-symbolic")
                .tooltip_text("Download photo")
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
        Some(NativeMediaAction::AnswerButton { .. } | NativeMediaAction::AnswerListRow { .. })
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
                let clip = gtk::MediaFile::for_filename(&path);
                clip.set_loop(true);
                clip.set_muted(true);
                clip.play();
                picture.set_paintable(Some(&clip));
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
            button.update_property(&[gtk::accessible::Property::Label("Play video")]);
            let details = ViewerDetails::of(message);
            button.connect_clicked(move |button| {
                if playable {
                    show_video(button, &path, &details, on_action.clone());
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
        Some(NativeMediaAction::AnswerButton { .. } | NativeMediaAction::AnswerListRow { .. })
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

/// Plays `path` in the viewer with sound; stops when the viewer closes and
/// offers another app when GStreamer cannot play it.
fn show_video(
    parent: &impl IsA<gtk::Widget>,
    path: &std::path::Path,
    details: &ViewerDetails,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let video = gtk::Video::builder()
        .file(&gtk::gio::File::for_path(path))
        .autoplay(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    let failed = adw::StatusPage::builder()
        .icon_name("video-x-generic-symbolic")
        .title("Can't Play This Video")
        .description("Open it with another app instead")
        .build();
    let stack = gtk::Stack::new();
    stack.add_named(&video, Some("video"));
    stack.add_named(&failed, Some("failed"));
    if let Some(stream) = video.media_stream() {
        let stack = stack.downgrade();
        let failed = failed.downgrade();
        stream.connect_error_notify(move |stream| {
            if let (Some(error), Some(stack), Some(failed)) =
                (stream.error(), stack.upgrade(), failed.upgrade())
            {
                // GStreamer names what is missing, such as a decoder.
                log::warn!("video could not be played: {error}");
                failed.set_description(Some(&format!(
                    "{error}\n\nOpen it with another app instead."
                )));
                stack.set_visible_child_name("failed");
            }
        });
    }
    let (open, target) = open_with_button(path, on_action);
    let dialog = media_viewer(
        parent,
        details,
        &stack,
        &[open.upcast(), show_in_folder_button(path).upcast()],
    )
    .dialog;
    *target.borrow_mut() = dialog.downgrade();
    dialog.connect_closed(move |_| {
        if let Some(stream) = video.media_stream() {
            stream.pause();
        }
    });
    dialog.present(Some(parent));
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
        .tooltip_text("Show in Folder")
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
                .tooltip_text("Open")
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
        Some(NativeMediaAction::AnswerButton { .. } | NativeMediaAction::AnswerListRow { .. })
        | None => actions.append(
            &adw::Spinner::builder()
                .width_request(24)
                .height_request(24)
                .valign(gtk::Align::Center)
                .tooltip_text("Downloading")
                .build(),
        ),
    }
    card.append(&actions);
    card
}

/// Who sent a photo or video, when, and its caption, for the viewer.
#[derive(Clone)]
struct ViewerDetails {
    title: String,
    subtitle: String,
    caption: Option<String>,
    thumbnail: Option<Vec<u8>>,
}

impl ViewerDetails {
    fn of(message: &Message) -> Self {
        let title = if message.from_me {
            "You".to_owned()
        } else {
            message
                .sender_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| {
                    crate::model::phone_of(&message.sender)
                        .map(crate::util::phone)
                        .unwrap_or_default()
                })
        };
        let caption = match &message.content {
            Content::Image { caption, .. } | Content::Video { caption, .. } => caption.clone(),
            _ => None,
        };
        Self {
            title,
            subtitle: crate::util::clock(message.timestamp),
            caption: caption.filter(|caption| !caption.is_empty()),
            thumbnail: message.thumbnail.clone(),
        }
    }
}

/// A dark, near-full-window dialog around `content`, with the sender and
/// time on top, the caption below, and `actions` in the header.
fn media_viewer(
    parent: &impl IsA<gtk::Widget>,
    details: &ViewerDetails,
    content: &impl IsA<gtk::Widget>,
    actions: &[gtk::Widget],
) -> Viewer {
    let title = adw::WindowTitle::new(&details.title, &details.subtitle);
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    for action in actions {
        header.pack_end(action);
    }
    let view = adw::ToolbarView::builder()
        .content(content)
        .top_bar_style(adw::ToolbarStyle::Raised)
        .css_classes(["zaptide-viewer"])
        .build();
    view.add_top_bar(&header);
    let caption = gtk::Label::builder()
        .label(details.caption.as_deref().unwrap_or_default())
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .max_width_chars(80)
        .justify(gtk::Justification::Center)
        .selectable(true)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(16)
        .margin_end(16)
        .build();
    view.add_bottom_bar(&caption);
    view.set_bottom_bar_style(adw::ToolbarStyle::Raised);
    view.set_reveal_bottom_bars(details.caption.is_some());
    // Most of the window, as a photo viewer would take.
    let (width, height) = parent
        .as_ref()
        .root()
        .map_or((900, 700), |root| (root.width(), root.height()));
    let dialog = adw::Dialog::builder()
        .title(&details.title)
        .content_width((width * 9 / 10).max(360))
        .content_height((height * 9 / 10).max(360))
        .child(&view)
        .build();
    Viewer {
        dialog,
        title,
        caption,
        view,
    }
}

/// A viewer dialog and the parts that change when it shows another item.
struct Viewer {
    dialog: adw::Dialog,
    title: adw::WindowTitle,
    caption: gtk::Label,
    view: adw::ToolbarView,
}

/// One photo the viewer can step to.
#[derive(Clone)]
struct PhotoItem {
    path: std::path::PathBuf,
    details: ViewerDetails,
}

/// A header button that closes `dialog` after opening the file elsewhere.
fn open_with_button(
    path: &std::path::Path,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) -> (
    gtk::Button,
    std::rc::Rc<std::cell::RefCell<glib::WeakRef<adw::Dialog>>>,
) {
    let button = gtk::Button::builder()
        .icon_name("adw-external-link-symbolic")
        .tooltip_text("Open With Another App")
        .build();
    let dialog: std::rc::Rc<std::cell::RefCell<glib::WeakRef<adw::Dialog>>> =
        std::rc::Rc::default();
    let (path, target) = (path.to_path_buf(), dialog.clone());
    button.connect_clicked(move |_| {
        on_action(NativeMediaAction::Open(path.clone()));
        if let Some(dialog) = target.borrow().upgrade() {
            dialog.close();
        }
    });
    (button, dialog)
}

/// Opens `path` in the viewer: fitted to the window, or at its real size
/// with double-click or the zoom button, and dragged around when larger.
fn show_photo(
    parent: &impl IsA<gtk::Widget>,
    items: std::rc::Rc<Vec<PhotoItem>>,
    start: usize,
    on_action: std::rc::Rc<dyn Fn(NativeMediaAction)>,
) {
    let start = start.min(items.len().saturating_sub(1));
    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label("Photo")]);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&picture)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let zoom = gtk::ToggleButton::builder()
        .icon_name("zoom-original-symbolic")
        .tooltip_text("Actual Size")
        .build();
    {
        let (picture, scroller) = (picture.clone(), scroller.clone());
        zoom.connect_toggled(move |zoom| {
            let actual = zoom.is_active();
            picture.set_can_shrink(!actual);
            let policy = if actual {
                gtk::PolicyType::Automatic
            } else {
                gtk::PolicyType::Never
            };
            scroller.set_policy(policy, policy);
            zoom.set_icon_name(if actual {
                "zoom-fit-best-symbolic"
            } else {
                "zoom-original-symbolic"
            });
            zoom.set_tooltip_text(Some(if actual {
                "Fit to Window"
            } else {
                "Actual Size"
            }));
        });
    }
    let double_click = gtk::GestureClick::new();
    {
        let zoom = zoom.clone();
        double_click.connect_pressed(move |_, presses, _, _| {
            if presses == 2 {
                zoom.set_active(!zoom.is_active());
            }
        });
    }
    scroller.add_controller(double_click);
    let pan = gtk::GestureDrag::new();
    let origin = std::rc::Rc::new(std::cell::Cell::new((0.0, 0.0)));
    {
        let (scroller, origin) = (scroller.clone(), origin.clone());
        pan.connect_drag_begin(move |_, _, _| {
            origin.set((
                scroller.hadjustment().value(),
                scroller.vadjustment().value(),
            ));
        });
    }
    {
        let scroller = scroller.clone();
        pan.connect_drag_update(move |_, x, y| {
            let (h, v) = origin.get();
            scroller.hadjustment().set_value(h - x);
            scroller.vadjustment().set_value(v - y);
        });
    }
    scroller.add_controller(pan);
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy Image")
        .build();
    {
        let picture = picture.clone();
        copy.connect_clicked(move |button| {
            if let Some(texture) = picture.paintable().and_downcast::<gdk::Texture>() {
                button.clipboard().set_texture(&texture);
            }
        });
    }
    // The header actions follow whichever photo is showing.
    let current = std::rc::Rc::new(std::cell::Cell::new(start));
    let dialog_ref: std::rc::Rc<std::cell::RefCell<glib::WeakRef<adw::Dialog>>> =
        std::rc::Rc::default();
    let open = gtk::Button::builder()
        .icon_name("adw-external-link-symbolic")
        .tooltip_text("Open With Another App")
        .build();
    {
        let (items, current, dialog_ref) = (items.clone(), current.clone(), dialog_ref.clone());
        open.connect_clicked(move |_| {
            on_action(NativeMediaAction::Open(items[current.get()].path.clone()));
            if let Some(dialog) = dialog_ref.borrow().upgrade() {
                dialog.close();
            }
        });
    }
    let folder = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .tooltip_text("Show in Folder")
        .valign(gtk::Align::Center)
        .build();
    {
        let (items, current) = (items.clone(), current.clone());
        folder.connect_clicked(move |button| show_in_folder(button, &items[current.get()].path));
    }
    let previous = photo_step_button("go-previous-symbolic", "Previous Photo", gtk::Align::Start);
    let next = photo_step_button("go-next-symbolic", "Next Photo", gtk::Align::End);
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    if items.len() > 1 {
        overlay.add_overlay(&previous);
        overlay.add_overlay(&next);
    }
    let viewer = media_viewer(
        parent,
        &items[start].details,
        &overlay,
        &[
            open.upcast(),
            folder.upcast(),
            copy.upcast(),
            zoom.clone().upcast(),
        ],
    );
    let dialog = viewer.dialog.clone();
    *dialog_ref.borrow_mut() = dialog.downgrade();
    // One token: showing another photo drops the previous one's pending load.
    let token = DecodeToken::default();
    let shown = current.clone();
    let show: std::rc::Rc<dyn Fn(usize)> = {
        let items = items.clone();
        let (picture, previous, next) = (picture.clone(), previous.clone(), next.clone());
        std::rc::Rc::new(move |index: usize| {
            current.set(index);
            let item = &items[index];
            zoom.set_active(false);
            // The sender's thumbnail shows at once, until the file loads.
            picture.set_paintable(
                item.details
                    .thumbnail
                    .as_deref()
                    .and_then(|bytes| gdk::Texture::from_bytes(&glib::Bytes::from(bytes)).ok())
                    .as_ref(),
            );
            // Full size up to a large screen; the view fits it to the dialog.
            load_photo(&picture, item.path.clone(), 3840, 3840, &token);
            viewer.title.set_title(&item.details.title);
            viewer.title.set_subtitle(&if items.len() > 1 {
                format!(
                    "{} · {} of {}",
                    item.details.subtitle,
                    index + 1,
                    items.len()
                )
            } else {
                item.details.subtitle.clone()
            });
            viewer
                .caption
                .set_label(item.details.caption.as_deref().unwrap_or_default());
            viewer
                .view
                .set_reveal_bottom_bars(item.details.caption.is_some());
            previous.set_sensitive(index > 0);
            next.set_sensitive(index + 1 < items.len());
        })
    };
    let step: std::rc::Rc<dyn Fn(isize)> = {
        let (show, items, shown) = (show.clone(), items.clone(), shown.clone());
        std::rc::Rc::new(move |delta: isize| {
            let target = shown.get().saturating_add_signed(delta);
            if delta != 0 && target < items.len() && target != shown.get() {
                show(target);
            }
        })
    };
    for (button, delta) in [(&previous, -1), (&next, 1)] {
        let step = step.clone();
        button.connect_clicked(move |_| step(delta));
    }
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |_, key, _, _| match key {
        gdk::Key::Left => {
            step(-1);
            glib::Propagation::Stop
        }
        gdk::Key::Right => {
            step(1);
            glib::Propagation::Stop
        }
        _ => glib::Propagation::Proceed,
    });
    dialog.add_controller(keys);
    show(start);
    dialog.present(Some(parent));
}

/// A round button over the viewer's edge that steps to a neighbouring photo.
fn photo_step_button(icon: &str, tooltip: &str, side: gtk::Align) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .halign(side)
        .valign(gtk::Align::Center)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["osd", "circular"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
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
        let texture = async {
            // Rows recycled while scrolling queue loads; skip the stale ones.
            if !current.is_current() {
                return None;
            }
            let mut loader = glycin::Loader::new(gtk::gio::File::for_path(&path));
            loader.accepted_memory_formats(glycin::MemoryFormatSelection::R8g8b8a8);
            let mut image = loader.load().await.inspect_err(photo_error).ok()?;
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
        .await;
        main_context.invoke(move || {
            if let (true, Some(picture), Some(texture)) =
                (ticket.is_current(), picture.upgrade(), texture)
            {
                picture.set_paintable(Some(&texture));
            }
        });
    });
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
