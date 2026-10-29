use std::{
    cell::{Cell, RefCell},
    path::Path,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

use anyhow::{Context, Result};
use gst::prelude::*;
use gst_video::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gtk::{gdk, glib, prelude::*, subclass::prelude::*};
use gtk4 as gtk;

#[derive(Default)]
struct Mailbox {
    sample: Option<gst::Sample>,
    waker: Option<Waker>,
    closed: bool,
}

impl Mailbox {
    fn publish(mailbox: &Mutex<Self>, sample: gst::Sample) {
        let waker = {
            let mut mailbox = mailbox.lock().unwrap_or_else(|e| e.into_inner());
            if mailbox.closed {
                return;
            }
            mailbox.sample = Some(sample);
            // Only the first frame wakes GTK until the receiver waits again.
            mailbox.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct VideoStream {
        pub(super) pipeline: RefCell<Option<gst::Element>>,
        pub(super) sink: RefCell<Option<gst_app::AppSink>>,
        pub(super) bus: RefCell<Option<gst::bus::BusWatchGuard>>,
        pub(super) task: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) mailbox: Arc<Mutex<Mailbox>>,
        pub(super) texture: RefCell<Option<gdk::MemoryTexture>>,
        pub(super) closed: Cell<bool>,
        pub(super) seeking: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VideoStream {
        const NAME: &'static str = "ZapTideVideoStream";
        type Type = super::VideoStream;
        type ParentType = gtk::MediaStream;
        type Interfaces = (gdk::Paintable,);
    }

    impl ObjectImpl for VideoStream {
        fn dispose(&self) {
            self.obj().close();
        }
    }

    impl MediaStreamImpl for VideoStream {
        fn play(&self) -> bool {
            self.pipeline.borrow().as_ref().is_some_and(|pipeline| {
                if self.obj().is_ended()
                    && pipeline
                        .seek_simple(
                            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                            gst::ClockTime::ZERO,
                        )
                        .is_err()
                {
                    return false;
                }
                pipeline.set_state(gst::State::Playing).is_ok()
            })
        }

        fn pause(&self) {
            if let Some(pipeline) = self.pipeline.borrow().as_ref() {
                let _ = pipeline.set_state(gst::State::Paused);
            }
        }

        fn seek(&self, timestamp: i64) {
            let success = self.pipeline.borrow().as_ref().is_some_and(|pipeline| {
                pipeline
                    .seek_simple(
                        gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                        gst::ClockTime::from_useconds(timestamp.max(0) as u64),
                    )
                    .is_ok()
            });
            self.seeking.set(success);
            if !success {
                self.obj().seek_failed();
            }
        }

        fn update_audio(&self, muted: bool, volume: f64) {
            if let Some(pipeline) = self.pipeline.borrow().as_ref() {
                pipeline.set_property("mute", muted);
                pipeline.set_property("volume", volume);
            }
        }

        fn realize(&self, _surface: gdk::Surface) {}
        fn unrealize(&self, _surface: gdk::Surface) {}
    }

    impl gdk::subclass::prelude::PaintableImpl for VideoStream {
        fn current_image(&self) -> gdk::Paintable {
            self.texture.borrow().as_ref().map_or_else(
                || gdk::Paintable::new_empty(0, 0),
                |texture| texture.clone().upcast(),
            )
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            if let Some(texture) = self.texture.borrow().as_ref() {
                texture.snapshot(snapshot, width, height);
            }
        }

        fn intrinsic_width(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |t| t.width())
        }

        fn intrinsic_height(&self) -> i32 {
            self.texture.borrow().as_ref().map_or(0, |t| t.height())
        }

        fn intrinsic_aspect_ratio(&self) -> f64 {
            self.texture
                .borrow()
                .as_ref()
                .map_or(0.0, |t| f64::from(t.width()) / f64::from(t.height()))
        }

        fn flags(&self) -> gdk::PaintableFlags {
            gdk::PaintableFlags::empty()
        }
    }
}

glib::wrapper! {
    /// A native video paintable with GTK playback controls and terminal shutdown.
    pub struct VideoStream(ObjectSubclass<imp::VideoStream>)
        @extends gtk::MediaStream, @implements gdk::Paintable;
}

impl VideoStream {
    /// Prepare a local file, bounding delivered frame dimensions in pixels.
    /// Call on the GTK thread; playback starts through `MediaStream::play`.
    pub(super) fn open(path: &Path, max_width: u32, max_height: u32) -> Result<Self> {
        gst::init().context("Video playback is unavailable")?;
        anyhow::ensure!(max_width > 0 && max_height > 0, "Invalid video bounds");
        let uri =
            glib::filename_to_uri(path, None).map_err(|_| anyhow::anyhow!("Cannot open video"))?;
        let pipeline = gst::ElementFactory::make("playbin")
            .build()
            .context("Video playback is unavailable")?;
        let sink = gst_app::AppSink::builder()
            .caps(
                &gst::Caps::builder("video/x-raw")
                    .field("format", "RGBA")
                    .field(
                        "width",
                        gst::IntRange::<i32>::new(1, max_width.min(i32::MAX as u32) as i32),
                    )
                    .field(
                        "height",
                        gst::IntRange::<i32>::new(1, max_height.min(i32::MAX as u32) as i32),
                    )
                    .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
                    .build(),
            )
            .max_buffers(1)
            .drop(true)
            .sync(true)
            .build();
        pipeline.set_property("uri", uri.as_str());
        pipeline.set_property("video-sink", &sink);
        let stream: Self = glib::Object::new();
        let imp = stream.imp();
        imp.pipeline.replace(Some(pipeline.clone()));
        imp.sink.replace(Some(sink.clone()));
        let samples = imp.mailbox.clone();
        let prerolls = samples.clone();
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Flushing)?;
                    Mailbox::publish(&samples, sample);
                    Ok(gst::FlowSuccess::Ok)
                })
                .new_preroll(move |sink| {
                    let sample = sink.pull_preroll().map_err(|_| gst::FlowError::Flushing)?;
                    Mailbox::publish(&prerolls, sample);
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        let weak = stream.downgrade();
        let mailbox = imp.mailbox.clone();
        imp.task
            .replace(Some(glib::MainContext::ref_thread_default().spawn_local(
                async move {
                    loop {
                        let sample = std::future::poll_fn(|cx| {
                            let mut mailbox = mailbox.lock().unwrap_or_else(|e| e.into_inner());
                            if mailbox.closed {
                                Poll::Ready(None)
                            } else if let Some(sample) = mailbox.sample.take() {
                                Poll::Ready(Some(sample))
                            } else {
                                mailbox.waker = Some(cx.waker().clone());
                                Poll::Pending
                            }
                        })
                        .await;
                        let (Some(stream), Some(sample)) = (weak.upgrade(), sample) else {
                            break;
                        };
                        stream.present(sample);
                    }
                },
            )));
        let weak = stream.downgrade();
        let bus = pipeline.bus().context("Video playback is unavailable")?;
        imp.bus.replace(Some(bus.add_watch_local(move |_, message| {
            let Some(stream) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            stream.handle_message(message);
            glib::ControlFlow::Continue
        })?));
        if pipeline.set_state(gst::State::Paused).is_err() {
            stream.close();
            anyhow::bail!("Cannot prepare video");
        }
        Ok(stream)
    }

    /// Release playback resources, even before preparation completes.
    /// Repeated calls are harmless; a closed stream cannot be reopened.
    pub(super) fn close(&self) {
        let imp = self.imp();
        if imp.closed.replace(true) {
            return;
        }
        {
            let mut mailbox = imp.mailbox.lock().unwrap_or_else(|e| e.into_inner());
            mailbox.closed = true;
            mailbox.sample = None;
            mailbox.waker = None;
        }
        if let Some(task) = imp.task.take() {
            task.abort();
        }
        imp.bus.take();
        if let Some(sink) = imp.sink.take() {
            sink.set_callbacks(gst_app::AppSinkCallbacks::builder().build());
        }
        if let Some(pipeline) = imp.pipeline.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
        self.set_playing(false);
        if self.is_prepared() {
            self.stream_unprepared();
        }
        imp.texture.take();
        self.invalidate_contents();
        self.invalidate_size();
    }

    fn present(&self, sample: gst::Sample) {
        if self.imp().closed.get() {
            return;
        }
        let Some(caps) = sample.caps() else {
            return;
        };
        let Ok(info) = gst_video::VideoInfo::from_caps(caps) else {
            return;
        };
        let Some(buffer) = sample.buffer() else {
            return;
        };
        let Ok(frame) = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info) else {
            return;
        };
        let Ok(data) = frame.plane_data(0) else {
            return;
        };
        let Ok(stride) = usize::try_from(frame.plane_stride()[0]) else {
            return;
        };
        let bytes = glib::Bytes::from_owned(data.to_vec());
        let texture = gdk::MemoryTexture::new(
            info.width() as i32,
            info.height() as i32,
            gdk::MemoryFormat::R8g8b8a8,
            &bytes,
            stride,
        );
        let changed =
            self.imp().texture.borrow().as_ref().is_none_or(|old| {
                old.width() != texture.width() || old.height() != texture.height()
            });
        self.imp().texture.replace(Some(texture));
        if changed {
            self.invalidate_size();
        }
        self.invalidate_contents();
        self.update_position();
    }

    fn update_position(&self) {
        if !self.is_prepared() {
            return;
        }
        // Timestamp notifications can close the stream and take the pipeline.
        let position = self
            .imp()
            .pipeline
            .borrow()
            .as_ref()
            .and_then(|pipeline| pipeline.query_position::<gst::ClockTime>());
        if let Some(position) = position {
            self.update(position.useconds().min(i64::MAX as u64) as i64);
        }
    }

    fn handle_message(&self, message: &gst::MessageRef) {
        if self.imp().closed.get() {
            return;
        }
        match message.view() {
            gst::MessageView::Error(_) => {
                self.close();
                self.set_error(glib::Error::new(
                    gst::ResourceError::Failed,
                    "Video playback failed",
                ));
            }
            gst::MessageView::Eos(_) => {
                let restarted = self.is_loop()
                    && self
                        .imp()
                        .pipeline
                        .borrow()
                        .as_ref()
                        .is_some_and(|pipeline| {
                            pipeline
                                .seek_simple(
                                    gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                                    gst::ClockTime::ZERO,
                                )
                                .is_ok()
                        });
                if !restarted {
                    self.stream_ended();
                }
            }
            gst::MessageView::AsyncDone(_) => {
                if !self.is_prepared() {
                    let metadata = self.imp().pipeline.borrow().as_ref().map(|pipeline| {
                        let duration = pipeline
                            .query_duration::<gst::ClockTime>()
                            .map_or(0, |time| time.useconds().min(i64::MAX as u64) as i64);
                        let mut query = gst::query::Seeking::new(gst::Format::Time);
                        let seekable = pipeline.query(&mut query) && query.result().0;
                        (
                            pipeline.property::<i32>("n-audio") > 0,
                            pipeline.property::<i32>("n-video") > 0,
                            seekable,
                            duration,
                        )
                    });
                    if let Some((audio, video, seekable, duration)) = metadata {
                        self.stream_prepared(audio, video, seekable, duration);
                    }
                }
                if self.imp().seeking.replace(false) {
                    self.seek_success();
                }
                self.update_position();
            }
            _ => {}
        }
    }
}
