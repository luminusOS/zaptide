use super::video_stream::VideoStream;
use super::*;
use crate::model::{Media, MediaState};
use relm4::typed_view::list::{RelmListItem, TypedListView};
use std::{
    cell::Cell,
    rc::Rc,
    time::{Duration, Instant},
};

struct CachedRow {
    chat: usize,
    media: Rc<NativeMediaWidget>,
    bound: Rc<Cell<bool>>,
}

impl RelmListItem for CachedRow {
    type Root = gtk::Box;
    type Widgets = ();

    fn setup(_: &gtk::ListItem) -> (Self::Root, ()) {
        (gtk::Box::new(gtk::Orientation::Vertical, 0), ())
    }

    fn bind(&mut self, _: &mut (), root: &mut gtk::Box) {
        root.append(&self.media.widget);
        self.bound.set(true);
    }

    fn unbind(&mut self, _: &mut (), root: &mut gtk::Box) {
        root.remove(&self.media.widget);
        self.bound.set(false);
    }
}

fn wait_until(reason: &str, mut ready: impl FnMut() -> bool) {
    let context = glib::MainContext::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "{reason}");
        context.iteration(false);
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn drain() {
    let deadline = Instant::now() + Duration::from_millis(150);
    wait_until("context drain", || Instant::now() >= deadline);
}

fn media_picture(media: &NativeMediaWidget) -> gtk::Picture {
    let child = media.widget.first_child().unwrap();
    let frame = child.downcast::<gtk::Overlay>().unwrap();
    let clamp = frame.child().unwrap().downcast::<adw::Clamp>().unwrap();
    clamp.child().unwrap().downcast::<gtk::Picture>().unwrap()
}

fn decoder(picture: &gtk::Picture) -> Option<VideoStream> {
    picture.paintable().and_downcast::<VideoStream>()
}

fn window(child: &impl IsA<gtk::Widget>) -> gtk::Window {
    gtk::Window::builder()
        .default_width(360)
        .default_height(240)
        .child(child)
        .build()
}

fn playing(picture: &gtk::Picture) -> VideoStream {
    wait_until("mapped GIF did not play", || {
        decoder(picture).is_some_and(|clip| {
            assert!(clip.error().is_none(), "{:?}", clip.error());
            clip.is_prepared() && clip.is_playing()
        })
    });
    let clip = decoder(picture).unwrap();
    assert!(clip.is_loop());
    assert!(clip.is_muted());
    wait_until("GIF did not deliver a bounded frame", || {
        clip.intrinsic_width() > 0 && clip.intrinsic_height() > 0
    });
    assert!(clip.intrinsic_width() <= 600);
    assert!(clip.intrinsic_height() <= 600);
    assert!((clip.intrinsic_aspect_ratio() - 16.0 / 9.0).abs() < 0.02);
    clip
}

fn released(picture: &gtk::Picture, thumbnail: &gdk::Paintable, clip: VideoStream) {
    assert!(!picture.is_mapped());
    assert!(!clip.is_playing(), "hidden GIF must stop");
    assert_eq!(picture.paintable().as_ref(), Some(thumbnail));
    let weak = clip.downgrade();
    drop(clip);
    drain();
    wait_until("hidden decoder retained", || weak.upgrade().is_none());
}

fn scroll_cached_gifs(message: &Message) {
    let mut list = TypedListView::<CachedRow, gtk::NoSelection>::new();
    let mut pictures = Vec::new();
    for _ in 0..24 {
        let media = Rc::new(build_media_widget_with_action(message, |_| {}));
        pictures.push(media_picture(&media));
        list.append(CachedRow {
            chat: 0,
            media,
            bound: Rc::default(),
        });
    }
    let scroll = gtk::ScrolledWindow::builder().child(&list.view).build();
    let window = window(&scroll);
    window.present();
    let adjustment = scroll.vadjustment();
    wait_until("GIF list did not become scrollable", || {
        adjustment.upper() > adjustment.page_size()
    });
    for position in [0.0, 0.5, 1.0, 0.0] {
        adjustment.set_value(position * (adjustment.upper() - adjustment.page_size()));
        drain();
        for picture in pictures.iter().filter(|picture| picture.is_mapped()) {
            drop(playing(picture));
        }
        let mut counts = [0usize; 5];
        for picture in &pictures {
            let mapped = picture.is_mapped();
            let playing = decoder(picture).is_some_and(|clip| clip.is_playing());
            let in_view = mapped
                && picture.compute_bounds(&scroll).is_some_and(|bounds| {
                    bounds.y() < scroll.height() as f32 && bounds.y() + bounds.height() > 0.0
                });
            counts[0] += usize::from(in_view);
            counts[1] += usize::from(mapped);
            counts[2] += usize::from(playing);
            counts[3] += usize::from(mapped && !in_view);
            counts[4] += usize::from(playing && !in_view);
            if !mapped {
                assert!(picture.paintable().unwrap().is::<gdk::Texture>());
            }
        }
        println!(
            "scroll={position:.1}, total=24: viewport={}, mapped={}, playing={}, offscreen_mapped={}, offscreen_playing={}",
            counts[0], counts[1], counts[2], counts[3], counts[4]
        );
        assert!(counts[0] > 0 && counts[0] < 24);
    }
    let decoders: Vec<_> = pictures
        .iter()
        .filter_map(|picture| decoder(picture).map(|clip| clip.downgrade()))
        .collect();
    window.destroy();
    drain();
    wait_until("scrolling list retained hidden decoders", || {
        decoders.iter().all(|clip| clip.upgrade().is_none())
    });
}

#[test]
#[ignore = "needs a display and GStreamer VP8 plugins"]
fn inline_gifs_release_playback_when_unmapped() {
    gtk::init().expect("display");
    adw::init().expect("libadwaita");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic.webm");
    let status = std::process::Command::new("gst-launch-1.0")
        .args("-q videotestsrc num-buffers=15 ! video/x-raw,width=640,height=360,framerate=15/1 ! vp8enc deadline=1 ! queue ! mux. audiotestsrc wave=silence num-buffers=45 ! audioconvert ! vorbisenc ! queue ! mux. webmmux name=mux ! filesink".split_whitespace())
        .arg(format!("location={}", path.display()))
        .status()
        .expect("GStreamer fixture generator");
    assert!(status.success());
    let mut message = super::tests::message(Content::Video {
        media: Media {
            mime: "video/webm".into(),
            size: 0,
            width: Some(640),
            height: Some(360),
            path: Some(path),
            state: MediaState::Idle,
        },
        seconds: Some(1),
        gif: true,
        caption: None,
    });
    let texture = gdk::MemoryTexture::new(
        2,
        2,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(vec![128u8; 16]),
        8,
    );
    message.thumbnail = Some(texture.save_to_png_bytes().to_vec());
    let media = Rc::new(build_media_widget_with_action(&message, |_| {}));
    let picture = media_picture(&media);
    let thumbnail = picture.paintable().expect("synthetic thumbnail");
    assert!(thumbnail.is::<gdk::Texture>());
    let bound = Rc::new(Cell::new(false));
    let mut list = TypedListView::<CachedRow, gtk::NoSelection>::new();
    list.append(CachedRow {
        chat: 0,
        media: media.clone(),
        bound: bound.clone(),
    });
    let chat = Rc::new(Cell::new(0));
    list.add_filter({
        let chat = chat.clone();
        move |row| row.chat == chat.get()
    });
    let window = window(&list.view);
    window.present();
    let first = playing(&picture);
    wait_until("GIF did not approach the loop boundary", || {
        first.timestamp() > 700_000
    });
    wait_until("GIF did not loop", || first.timestamp() < 400_000);
    window.set_visible(false);
    released(&picture, &thumbnail, first);
    window.present();
    drop(playing(&picture));

    #[cfg(target_os = "linux")]
    let mut fd_samples = Vec::new();
    for batch in 0..4 {
        for _ in 0..4 {
            let clip = playing(&picture);
            assert!(bound.get());
            chat.set(1);
            assert!(list.notify_filter_changed(0));
            wait_until("chat did not unbind", || !bound.get());
            released(&picture, &thumbnail, clip);
            chat.set(0);
            assert!(list.notify_filter_changed(0));
            wait_until("cached row did not rebind", || bound.get());
            drop(playing(&picture));
        }
        window.set_visible(false);
        drain();
        #[cfg(target_os = "linux")]
        fd_samples.push(std::fs::read_dir("/proc/self/fd").unwrap().count());
        println!("playback batch {batch} drained");
        window.present();
        drop(playing(&picture));
    }
    #[cfg(target_os = "linux")]
    {
        println!("post-drain FD samples (first batch is warmup): {fd_samples:?}");
        assert!(
            fd_samples[3] <= fd_samples[1] + 4,
            "FDs grew across warmed batches: {fd_samples:?}"
        );
    }
    window.destroy();
    drain();
    for _ in 0..8 {
        let media = build_media_widget_with_action(&message, |_| {});
        let picture = media_picture(&media);
        let thumbnail = picture.paintable().unwrap();
        let window = gtk::Window::builder().child(&media.widget).build();
        window.present();
        wait_until("rapid teardown never mapped", || picture.is_mapped());
        let clip = decoder(&picture).unwrap();
        assert!(
            !clip.is_prepared(),
            "rapid teardown must precede preparation"
        );
        window.destroy();
        released(&picture, &thumbnail, clip);
    }
    scroll_cached_gifs(&message);
    let Content::Video { media, .. } = &message.content else {
        unreachable!()
    };
    video_controls_and_errors(media.path.as_ref().unwrap(), directory.path());
}

fn video_controls_and_errors(path: &std::path::Path, directory: &std::path::Path) {
    let clip = VideoStream::open(path, 320, 180).unwrap();
    let video = gtk::Video::builder().media_stream(&clip).build();
    let window = window(&video);
    window.present();
    wait_until("viewer did not prepare", || {
        assert!(clip.error().is_none(), "{:?}", clip.error());
        clip.is_prepared() && clip.intrinsic_width() > 0
    });
    assert!(clip.has_video());
    assert!(clip.has_audio());
    assert!(clip.is_seekable());
    assert!(clip.duration() > 0);
    assert!(clip.intrinsic_width() <= 320);
    assert!(clip.intrinsic_height() <= 180);
    assert!((clip.intrinsic_aspect_ratio() - 16.0 / 9.0).abs() < 0.02);
    clip.set_volume(0.25);
    assert_eq!(clip.volume(), 0.25);
    clip.play();
    wait_until("viewer playback did not advance", || clip.timestamp() > 0);
    clip.pause();
    assert!(!clip.is_playing());
    clip.seek(500_000);
    wait_until("viewer seek did not finish", || !clip.is_seeking());
    assert!(clip.timestamp() >= 400_000);
    clip.play();
    wait_until("viewer did not reach EOS", || clip.is_ended());
    clip.play();
    wait_until("viewer did not restart after EOS", || {
        clip.timestamp() > 0 && clip.timestamp() < 400_000
    });
    clip.close();
    clip.close();
    assert!(!clip.is_playing());
    window.destroy();
    window.set_child(gtk::Widget::NONE);
    drop(video);
    let weak = clip.downgrade();
    drop(clip);
    wait_until("viewer retained closed stream", || weak.upgrade().is_none());

    let invalid = directory.join("invalid.webm");
    std::fs::write(&invalid, b"synthetic invalid video").unwrap();
    let clip = VideoStream::open(&invalid, 320, 180).unwrap();
    clip.play();
    wait_until("invalid video did not report an error", || {
        clip.error().is_some()
    });
    clip.close();
    clip.close();

    let clip = VideoStream::open(path, 320, 180).unwrap();
    clip.connect_timestamp_notify(|clip| {
        if clip.timestamp() > 0 {
            clip.close();
        }
    });
    wait_until("reentrant-close stream did not prepare", || {
        clip.is_prepared()
    });
    clip.play();
    wait_until("timestamp handler did not close stream", || {
        !clip.is_prepared()
    });
    let weak = clip.downgrade();
    drop(clip);
    wait_until("timestamp handler retained stream", || {
        weak.upgrade().is_none()
    });
}
