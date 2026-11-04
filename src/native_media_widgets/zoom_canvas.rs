use std::{
    cell::{Cell, RefCell},
    sync::OnceLock,
};

use relm4::gtk::{self, gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};

use super::zoom::{self, Level};

mod imp {
    use super::*;

    pub struct ZoomCanvas {
        pub(super) texture: RefCell<Option<gdk::Texture>>,
        pub(super) picture: Cell<(f64, f64)>,
        pub(super) level: Cell<Level>,
    }

    impl Default for ZoomCanvas {
        fn default() -> Self {
            Self {
                texture: RefCell::default(),
                picture: Cell::new((1.0, 1.0)),
                level: Cell::new(Level::Fit),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ZoomCanvas {
        const NAME: &'static str = "ZapTideZoomCanvas";
        type Type = super::ZoomCanvas;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    impl ObjectImpl for ZoomCanvas {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("layout-changed").build()])
        }
    }

    impl ZoomCanvas {
        pub(super) fn scale_in(&self, width: i32, height: i32) -> f64 {
            match self.level.get() {
                Level::Scale(scale) => scale,
                Level::Fit => {
                    zoom::fit_scale(self.picture.get(), (f64::from(width), f64::from(height)))
                }
            }
        }
    }

    impl WidgetImpl for ZoomCanvas {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let (width, height) = self.picture.get();
            let natural = if orientation == gtk::Orientation::Horizontal {
                width
            } else {
                height
            };
            match self.level.get() {
                Level::Fit => (0, natural.ceil() as i32, -1, -1),
                Level::Scale(scale) => {
                    let size = (natural * scale).ceil() as i32;
                    (size, size, -1, -1)
                }
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().emit_by_name::<()>("layout-changed", &[]);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (width, height) = (widget.width(), widget.height());
            let texture = self.texture.borrow();
            let Some(texture) = texture.as_ref() else {
                return;
            };
            let scale = self.scale_in(width, height);
            let (picture_width, picture_height) = self.picture.get();
            let (shown_width, shown_height) = (
                (picture_width * scale).round().max(1.0),
                (picture_height * scale).round().max(1.0),
            );
            let x = ((f64::from(width) - shown_width) / 2.0).floor();
            let y = ((f64::from(height) - shown_height) / 2.0).floor();
            let magnification = shown_width / f64::from(texture.width().max(1));
            let filter = if magnification >= 3.0 {
                gsk::ScalingFilter::Nearest
            } else if magnification < 0.5 {
                gsk::ScalingFilter::Trilinear
            } else {
                gsk::ScalingFilter::Linear
            };
            snapshot.append_scaled_texture(
                texture,
                filter,
                &graphene::Rect::new(x as f32, y as f32, shown_width as f32, shown_height as f32),
            );
        }
    }
}

glib::wrapper! {
    /// A picture drawn at any scale from a texture that may have fewer pixels
    /// than the picture it stands for, meant to sit in a scrolled window. At
    /// `Level::Fit` it follows its allocation; at a scale it asks for exactly
    /// that size and centres the picture when the view is larger.
    pub struct ZoomCanvas(ObjectSubclass<imp::ZoomCanvas>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ZoomCanvas {
    pub(super) fn new() -> Self {
        glib::Object::builder()
            .property("hexpand", true)
            .property("vexpand", true)
            .build()
    }

    /// Shows `texture` as a picture of `picture` pixels.
    pub(super) fn set_picture(&self, texture: Option<gdk::Texture>, picture: (f64, f64)) {
        let imp = self.imp();
        let resized = imp.picture.replace(picture) != picture;
        imp.texture.replace(texture);
        if resized {
            self.queue_resize();
        }
        self.queue_draw();
    }

    pub(super) fn set_level(&self, level: Level) {
        if self.imp().level.replace(level) != level {
            self.queue_resize();
            self.queue_draw();
        }
    }

    pub(super) fn level(&self) -> Level {
        self.imp().level.get()
    }

    pub(super) fn picture(&self) -> (f64, f64) {
        self.imp().picture.get()
    }

    /// The scale the picture is drawn at now.
    pub(super) fn scale(&self) -> f64 {
        self.imp().scale_in(self.width(), self.height())
    }

    pub(super) fn connect_layout_changed(&self, handler: impl Fn(&Self) + 'static) {
        self.connect_local("layout-changed", false, move |values| {
            if let Ok(canvas) = values[0].get::<Self>() {
                handler(&canvas);
            }
            None
        });
    }
}
