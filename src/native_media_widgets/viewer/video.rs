use super::*;

const SEEK_STEP: f64 = 5_000_000.0;
const SEEK_PAGE: f64 = 10_000_000.0;

pub(super) fn seek_target(value: f64, duration: i64) -> i64 {
    (value.max(0.0) as i64).min(duration.max(0))
}

pub(super) fn video_label(name: &str) -> String {
    gettext("Video from {name}").replace("{name}", name)
}

pub(super) fn clock_text(micros: i64) -> String {
    crate::util::duration(u32::try_from(micros.max(0) / 1_000_000).unwrap_or(u32::MAX))
}

pub(super) fn volume_icon(volume: f64, muted: bool) -> &'static str {
    if muted || volume <= 0.0 {
        "audio-volume-muted-symbolic"
    } else if volume < 0.34 {
        "audio-volume-low-symbolic"
    } else if volume < 0.67 {
        "audio-volume-medium-symbolic"
    } else {
        "audio-volume-high-symbolic"
    }
}

struct VideoControls {
    bar: adw::Clamp,
    big_play: gtk::Button,
}

/// The floating play, progress, volume and fullscreen controls of `clip`,
/// and the large play button shown while it is paused. The widgets hold the
/// stream weakly. The page lifetime stops playback and restores fullscreen.
fn video_controls(
    clip: &VideoStream,
    page: &MediaViewer,
    parent: &impl IsA<gtk::Widget>,
) -> VideoControls {
    let window = parent.as_ref().root().and_downcast::<gtk::Window>();
    let play = header_button("media-playback-start-symbolic", &gettext("Play"));
    let position = label_with(&["caption", "numeric"]);
    let length = label_with(&["caption", "dim-label", "numeric"]);
    let seek = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .adjustment(&gtk::Adjustment::new(
            0.0, 0.0, 1.0, SEEK_STEP, SEEK_PAGE, 0.0,
        ))
        .draw_value(false)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["zaptide-video-seek"])
        .build();
    seek.update_property(&[gtk::accessible::Property::Label(&gettext("Seek"))]);

    let volume_scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Vertical)
        .adjustment(&gtk::Adjustment::new(
            clip.volume(),
            0.0,
            1.0,
            0.05,
            0.1,
            0.0,
        ))
        .draw_value(false)
        .inverted(true)
        .height_request(120)
        .build();
    volume_scale.update_property(&[gtk::accessible::Property::Label(&gettext("Volume"))]);
    let volume = gtk::MenuButton::builder()
        .icon_name(volume_icon(clip.volume(), clip.is_muted()))
        .tooltip_text(gettext("Volume"))
        .direction(gtk::ArrowType::Up)
        .valign(gtk::Align::Center)
        .popover(&gtk::Popover::builder().child(&volume_scale).build())
        .css_classes(["flat"])
        .build();
    volume.update_property(&[gtk::accessible::Property::Label(&gettext("Volume"))]);
    let fullscreen = header_button("view-fullscreen-symbolic", &gettext("Fullscreen"));
    let set_fullscreen_look = |button: &gtk::Button, on: bool| {
        let (icon, label) = if on {
            ("view-restore-symbolic", gettext("Exit Fullscreen"))
        } else {
            ("view-fullscreen-symbolic", gettext("Fullscreen"))
        };
        button.set_icon_name(icon);
        button.set_tooltip_text(Some(&label));
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
    };
    if window.as_ref().is_some_and(|window| window.is_fullscreen()) {
        set_fullscreen_look(&fullscreen, true);
    }

    let row = gtk::Box::builder()
        .spacing(10)
        .css_classes(["zaptide-video-controls"])
        .build();
    for widget in [
        play.upcast_ref::<gtk::Widget>(),
        position.upcast_ref(),
        seek.upcast_ref(),
        length.upcast_ref(),
        volume.upcast_ref(),
        fullscreen.upcast_ref(),
    ] {
        row.append(widget);
    }
    let bar = adw::Clamp::builder()
        .maximum_size(640)
        .tightening_threshold(480)
        .child(&row)
        .valign(gtk::Align::End)
        .margin_start(16)
        .margin_end(16)
        .margin_bottom(16)
        .visible(false)
        .build();

    let big_play = gtk::Button::builder()
        .icon_name("media-playback-start-symbolic")
        .tooltip_text(gettext("Play"))
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .css_classes(["circular", "zaptide-play", "zaptide-video-play"])
        .visible(false)
        .build();
    big_play.update_property(&[gtk::accessible::Property::Label(&gettext("Play"))]);

    play.connect_clicked({
        let clip = clip.downgrade();
        move |_| {
            if let Some(clip) = clip.upgrade() {
                if clip.is_playing() {
                    clip.pause();
                } else {
                    clip.play();
                }
            }
        }
    });
    big_play.connect_clicked({
        let clip = clip.downgrade();
        move |_| {
            if let Some(clip) = clip.upgrade() {
                clip.play();
            }
        }
    });

    // A drag moves the handle and the time only; the video seeks once, where
    // the handle is let go. Keys and the wheel seek at once.
    let dragging = Rc::new(Cell::new(false));
    let finish_drag: Rc<dyn Fn()> = Rc::new({
        let (clip, seek, dragging) = (clip.downgrade(), seek.downgrade(), dragging.clone());
        move || {
            if !dragging.replace(false) {
                return;
            }
            if let (Some(clip), Some(seek)) = (clip.upgrade(), seek.upgrade())
                && clip.is_seekable()
            {
                clip.seek(seek_target(seek.value(), clip.duration()));
            }
        }
    });
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    drag.connect_drag_begin({
        let dragging = dragging.clone();
        move |_, _, _| dragging.set(true)
    });
    drag.connect_drag_end({
        let finish_drag = finish_drag.clone();
        move |_, _, _| finish_drag()
    });
    drag.connect_cancel(move |_, _| finish_drag());
    seek.add_controller(drag);
    seek.connect_change_value({
        let (clip, position, dragging) = (clip.downgrade(), position.downgrade(), dragging.clone());
        move |_, _, value| {
            if let Some(clip) = clip.upgrade() {
                let target = seek_target(value, clip.duration());
                if dragging.get() {
                    if let Some(position) = position.upgrade() {
                        position.set_label(&clock_text(target));
                    }
                } else if clip.is_seekable() {
                    clip.seek(target);
                }
            }
            glib::Propagation::Proceed
        }
    });
    volume_scale.connect_value_changed({
        let clip = clip.downgrade();
        move |scale| {
            if let Some(clip) = clip.upgrade() {
                clip.set_muted(false);
                clip.set_volume(scale.value());
            }
        }
    });

    fullscreen.connect_clicked({
        let window = window.as_ref().map(ObjectExt::downgrade);
        move |button| {
            let Some(window) = window.as_ref().and_then(glib::WeakRef::upgrade) else {
                return;
            };
            let enter = !window.is_fullscreen();
            window.set_fullscreened(enter);
            set_fullscreen_look(button, enter);
        }
    });
    let was_fullscreen = window.as_ref().is_some_and(|window| window.is_fullscreen());
    page.lifetime.borrow_mut().on_close({
        let window = window.as_ref().map(ObjectExt::downgrade);
        move || {
            if let Some(window) = window.as_ref().and_then(glib::WeakRef::upgrade) {
                window.set_fullscreened(was_fullscreen);
            }
        }
    });

    let refresh = Rc::new(glib::clone!(
        #[weak]
        play,
        #[weak]
        position,
        #[weak]
        length,
        #[weak]
        seek,
        #[weak]
        volume,
        #[weak]
        bar,
        #[weak]
        big_play,
        move |clip: &VideoStream| {
            let ready = clip.is_prepared();
            let playing = clip.is_playing();
            if bar.is_visible() != ready {
                bar.set_visible(ready);
            }
            let show_big = ready && !playing;
            if big_play.is_visible() != show_big {
                big_play.set_visible(show_big);
            }
            let (icon, label) = if playing {
                ("media-playback-pause-symbolic", gettext("Pause"))
            } else {
                ("media-playback-start-symbolic", gettext("Play"))
            };
            if play.icon_name().as_deref() != Some(icon) {
                play.set_icon_name(icon);
                play.set_tooltip_text(Some(&label));
                play.update_property(&[gtk::accessible::Property::Label(&label)]);
            }
            let upper = clip.duration().max(1) as f64;
            let adjustment = seek.adjustment();
            if adjustment.upper() != upper {
                adjustment.set_upper(upper);
            }
            let seekable = clip.is_seekable();
            if seek.is_sensitive() != seekable {
                seek.set_sensitive(seekable);
            }
            if !dragging.get() {
                seek.set_value(clip.timestamp() as f64);
                let elapsed = clock_text(clip.timestamp());
                let total = clock_text(clip.duration());
                if position.label().as_str() != elapsed || length.label().as_str() != total {
                    position.set_label(&elapsed);
                    length.set_label(&total);
                    seek.update_property(&[gtk::accessible::Property::ValueText(&of_text(
                        &elapsed, &total,
                    ))]);
                }
            }
            volume.set_icon_name(volume_icon(clip.volume(), clip.is_muted()));
        }
    ));
    clip.connect_notify_local(None, {
        let refresh = refresh.clone();
        move |clip, _| refresh(clip)
    });
    refresh(clip);
    VideoControls { bar, big_play }
}

/// Plays `path` in the viewer with sound; stops when the viewer closes and
/// offers another app when GStreamer cannot play it.
pub(crate) fn build_video(
    parent: &impl IsA<gtk::Widget>,
    path: &std::path::Path,
    details: &ViewerDetails,
    on_action: Rc<dyn Fn(NativeMediaAction)>,
    on_close: Rc<dyn Fn()>,
) -> MediaViewer {
    let clip = VideoStream::open_in_background(path, 1920, 1080, {
        let (path, on_action, on_close) = (path.to_path_buf(), on_action.clone(), on_close.clone());
        move || {
            on_action(NativeMediaAction::Open(path));
            on_close();
        }
    });
    let picture = gtk::Picture::builder()
        .paintable(&clip)
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .hexpand(true)
        .vexpand(true)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label(&video_label(
        &details.title,
    ))]);
    let failed = adw::StatusPage::builder()
        .icon_name("video-x-generic-symbolic")
        .title(gettext("Can't Play This Video"))
        .description(gettext("Open it with another app instead"))
        .build();
    let another = gtk::Button::builder()
        .label(gettext("Open in Another App"))
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .build();
    failed.set_child(Some(&another));

    let folder = header_button("folder-open-symbolic", &gettext("Show in Folder"));
    folder.connect_clicked({
        let target = path.to_path_buf();
        move |button| show_in_folder(button, &target)
    });
    let open = header_button(
        "adw-external-link-symbolic",
        &gettext("Open With Another App"),
    );
    let viewer = media_viewer(
        details,
        &gtk::Box::new(gtk::Orientation::Vertical, 0),
        None,
        &[folder.upcast(), open.clone().upcast()],
        on_close,
    );
    let page = viewer.page;
    let lifetime = keep_while_open(&page, [path.to_path_buf()]);
    open.connect_clicked(open_and_close(path, &on_action, &page.on_close));
    another.connect_clicked(open_and_close(path, &on_action, &page.on_close));

    let controls = video_controls(&clip, &page, parent);
    let overlay = gtk::Overlay::builder().child(&picture).build();
    let waiting = video_placeholder(details, &clip);
    overlay.add_overlay(&waiting);
    overlay.add_overlay(&controls.big_play);
    overlay.add_overlay(&controls.bar);
    let stack = gtk::Stack::new();
    stack.add_named(&overlay, Some("video"));
    stack.add_named(&failed, Some("failed"));
    viewer.view.set_content(Some(&stack));

    clip.connect_error_notify(glib::clone!(
        #[weak]
        stack,
        move |stream| {
            if let Some(error) = stream.error() {
                log::warn!("video could not be played: {error}");
                stack.set_visible_child_name("failed");
            }
        }
    ));
    if clip.is_prepared() {
        clip.play();
    } else {
        clip.connect_prepared_notify(|clip| {
            if clip.is_prepared() {
                clip.play();
            }
        });
    }
    lifetime.borrow_mut().on_close(move || clip.close());
    page
}

#[cfg(test)]
pub(super) fn show_video(
    parent: &impl IsA<gtk::Widget>,
    path: &std::path::Path,
    details: &ViewerDetails,
    on_action: Rc<dyn Fn(NativeMediaAction)>,
) {
    let (host, on_close) = test_host(parent);
    let page = build_video(parent, path, details, on_action, on_close);
    present_test_page(&host, page);
}

/// The sender's thumbnail with a spinner over it, shown until the clip has a
/// frame of its own or fails.
fn video_placeholder(details: &ViewerDetails, clip: &VideoStream) -> gtk::Overlay {
    let poster = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();
    poster.set_paintable(
        details
            .thumbnail
            .as_deref()
            .and_then(super::inline_thumbnail_texture)
            .as_ref(),
    );
    let placeholder = gtk::Overlay::builder()
        .child(&poster)
        .can_target(false)
        .build();
    placeholder.add_overlay(
        &adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build(),
    );
    let update = glib::clone!(
        #[weak]
        placeholder,
        move |clip: &VideoStream| {
            let shown =
                clip.error().is_none() && !(clip.is_prepared() && clip.intrinsic_width() > 0);
            placeholder.set_visible(shown);
        }
    );
    let update = Rc::new(update);
    clip.connect_invalidate_size({
        let update = update.clone();
        move |clip| update(clip)
    });
    clip.connect_notify_local(None, move |clip, _| update(clip));
    placeholder
}

fn open_and_close(
    path: &std::path::Path,
    on_action: &Rc<dyn Fn(NativeMediaAction)>,
    on_close: &Rc<dyn Fn()>,
) -> impl Fn(&gtk::Button) + 'static {
    let (path, on_action, on_close) = (path.to_path_buf(), on_action.clone(), on_close.clone());
    move |_| {
        on_action(NativeMediaAction::Open(path.clone()));
        on_close();
    }
}
