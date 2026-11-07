use super::*;

fn step_button(icon: &str, tooltip: &str, side: gtk::Align) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(tooltip)
        .halign(side)
        .valign(gtk::Align::Center)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["circular", "zaptide-viewer-nav"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    button
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ZoomKey {
    In,
    Out,
    Fit,
    Actual,
}

pub(super) fn zoom_key(key: gdk::Key, state: gdk::ModifierType) -> Option<ZoomKey> {
    let held =
        gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK | gdk::ModifierType::META_MASK;
    if state.intersects(held) {
        return None;
    }
    match key {
        gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => Some(ZoomKey::In),
        gdk::Key::minus | gdk::Key::KP_Subtract => Some(ZoomKey::Out),
        gdk::Key::_0 | gdk::Key::KP_0 => Some(ZoomKey::Fit),
        gdk::Key::_1 | gdk::Key::KP_1 => Some(ZoomKey::Actual),
        _ => None,
    }
}

pub(super) fn pan_key(key: gdk::Key, state: gdk::ModifierType) -> Option<(f64, f64)> {
    let held = gdk::ModifierType::SHIFT_MASK
        | gdk::ModifierType::CONTROL_MASK
        | gdk::ModifierType::ALT_MASK
        | gdk::ModifierType::SUPER_MASK
        | gdk::ModifierType::META_MASK;
    if state.intersects(held) {
        return None;
    }
    match key {
        gdk::Key::Left => Some((-1.0, 0.0)),
        gdk::Key::Right => Some((1.0, 0.0)),
        gdk::Key::Up => Some((0.0, -1.0)),
        gdk::Key::Down => Some((0.0, 1.0)),
        _ => None,
    }
}

fn can_scroll(adjustment: &gtk::Adjustment) -> bool {
    adjustment.upper() - adjustment.page_size() > 0.5
}

type WaitingCopy = Box<dyn FnOnce(&gdk::Texture)>;

/// What the photo viewer is showing. Widgets that own a handler for it are
/// held weakly, so it never keeps the dialog alive.
pub(super) struct PhotoState {
    items: Rc<Vec<PhotoItem>>,
    current: Cell<usize>,
    pub(super) canvas: ZoomCanvas,
    pub(super) scroller: glib::WeakRef<gtk::ScrolledWindow>,
    pill: ZoomPillWeak,
    copy: glib::WeakRef<gtk::Button>,
    previous: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    parts: ViewerParts,
    /// Size of the first decode: what the viewer needs to show the photo
    /// fitted, not the photo itself.
    first_decode: (u32, u32),
    /// A photo has loaded: zoom and copy wait for it, so neither acts on the
    /// sender's tiny thumbnail.
    pub(super) ready: Cell<bool>,
    texture: RefCell<Option<gdk::Texture>>,
    /// Smallest density a decode in flight will reach, if one is.
    loading: Cell<Option<f64>>,
    /// Copies waiting for the photo at its real size.
    waiting: RefCell<Vec<WaitingCopy>>,
    token: DecodeToken,
}

impl PhotoState {
    fn item(&self) -> &PhotoItem {
        &self.items[self.current.get()]
    }

    pub(super) fn view_size(&self) -> (f64, f64) {
        self.scroller.upgrade().map_or((0.0, 0.0), |scroller| {
            (
                scroller.hadjustment().page_size(),
                scroller.vadjustment().page_size(),
            )
        })
    }

    fn bounds(&self) -> Bounds {
        Bounds::new(self.canvas.picture(), self.view_size())
    }

    fn pannable(&self) -> (bool, bool) {
        if self.canvas.level() == Level::Fit {
            return (false, false);
        }
        self.scroller.upgrade().map_or((false, false), |scroller| {
            (
                can_scroll(&scroller.hadjustment()),
                can_scroll(&scroller.vadjustment()),
            )
        })
    }

    fn sync_controls(&self) {
        let ready = self.ready.get();
        let bounds = self.bounds();
        let scale = self.canvas.scale();
        self.pill.show(&ZoomView {
            percent: zoom::percent(scale),
            at_fit: self.canvas.level() == Level::Fit,
            can_shrink: bounds.can_shrink(scale),
            can_grow: bounds.can_grow(scale),
            ready,
        });
        if let Some(copy) = self.copy.upgrade() {
            copy.set_sensitive(ready);
        }
        if let Some(scroller) = self.scroller.upgrade() {
            let (across, down) = self.pannable();
            scroller.set_cursor_from_name((across || down).then_some("grab"));
        }
    }

    pub(super) fn texture_size(&self) -> (u32, u32) {
        self.texture.borrow().as_ref().map_or((0, 0), |texture| {
            (
                u32::try_from(texture.width()).unwrap_or(0),
                u32::try_from(texture.height()).unwrap_or(0),
            )
        })
    }

    fn real_size(&self) -> (u32, u32) {
        zoom::decode_size(self.canvas.picture(), 1.0, NATIVE_PIXELS)
    }

    fn has_real_size(&self) -> bool {
        let (have, real) = (self.texture_size(), self.real_size());
        have.0 + 1 >= real.0 && have.1 + 1 >= real.1
    }

    fn show(self: &Rc<Self>, index: usize) {
        self.current.set(index);
        self.ready.set(false);
        self.loading.set(None);
        self.waiting.take();
        self.texture.take();
        let item = &self.items[index];
        let thumbnail = item
            .details
            .thumbnail
            .as_deref()
            .and_then(super::inline_thumbnail_texture);
        let picture = thumbnail.as_ref().map_or((1.0, 1.0), |texture| {
            zoom::logical_size(
                (
                    u32::try_from(texture.width()).unwrap_or(1),
                    u32::try_from(texture.height()).unwrap_or(1),
                ),
                item.details.dimensions,
            )
        });
        self.canvas.set_level(Level::Fit);
        self.canvas.set_picture(thumbnail, picture);
        if let Some(scroller) = self.scroller.upgrade() {
            scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
            scroller.hadjustment().set_value(0.0);
            scroller.vadjustment().set_value(0.0);
        }
        self.sync_controls();
        self.canvas
            .update_property(&[gtk::accessible::Property::Label(&photo_label(
                &item.details.title,
            ))]);
        self.parts
            .show(&item.details, Some((index, self.items.len())));
        if let Some(previous) = self.previous.upgrade() {
            previous.set_sensitive(index > 0);
        }
        if let Some(next) = self.next.upgrade() {
            next.set_sensitive(index + 1 < self.items.len());
        }
        let declared = item.details.dimensions;
        let state = self.clone();
        decode_photo(
            item.path.clone(),
            self.first_decode.0,
            self.first_decode.1,
            &self.token,
            move |texture| {
                let size = (
                    u32::try_from(texture.width()).unwrap_or(1),
                    u32::try_from(texture.height()).unwrap_or(1),
                );
                let picture = zoom::logical_size(size, declared);
                state.canvas.set_picture(Some(texture.clone()), picture);
                state.texture.replace(Some(texture));
                state.ready.set(true);
                state.sync_controls();
                state.refine();
            },
        );
    }

    fn step(self: &Rc<Self>, delta: isize) {
        if let Some(target) = step_target(self.current.get(), delta, self.items.len()) {
            self.show(target);
        }
    }

    /// Zooms to `level`, keeping the point of the photo under `anchor` (a
    /// position in the view, by default its middle) where it is.
    fn set_level(self: &Rc<Self>, level: Level, anchor: Option<(f64, f64)>) {
        let Some(scroller) = self.scroller.upgrade().filter(|_| self.ready.get()) else {
            return;
        };
        let view = self.view_size();
        let picture = self.canvas.picture();
        let bounds = Bounds::new(picture, view);
        let level = match level {
            Level::Fit => Level::Fit,
            Level::Scale(scale) => bounds.level(scale),
        };
        if level == self.canvas.level() {
            return;
        }
        let from = self.canvas.scale();
        let to = match level {
            Level::Fit => bounds.fit,
            Level::Scale(scale) => scale,
        };
        let (across, down) = (scroller.hadjustment(), scroller.vadjustment());
        let anchor = anchor.unwrap_or((view.0 / 2.0, view.1 / 2.0));
        let offsets = (
            zoom::anchored_offset(picture.0, view.0, from, to, across.value(), anchor.0),
            zoom::anchored_offset(picture.1, view.1, from, to, down.value(), anchor.1),
        );
        self.canvas.set_level(level);
        let policy = if level == Level::Fit {
            gtk::PolicyType::Never
        } else {
            gtk::PolicyType::Automatic
        };
        scroller.set_policy(policy, policy);
        if level == Level::Fit {
            across.set_value(0.0);
            down.set_value(0.0);
        } else {
            across.set_upper(zoom::content_len(picture.0, view.0, to));
            down.set_upper(zoom::content_len(picture.1, view.1, to));
            across.set_value(offsets.0);
            down.set_value(offsets.1);
        }
        self.sync_controls();
        self.refine();
    }

    pub(super) fn zoom_by(self: &Rc<Self>, zoom_in: bool) {
        let level = self.bounds().stepped(self.canvas.scale(), zoom_in);
        self.set_level(level, None);
    }

    pub(super) fn toggle_fit(self: &Rc<Self>, anchor: Option<(f64, f64)>) {
        self.set_level(self.canvas.level().toggled(), anchor);
    }

    /// Holding Ctrl, the wheel zooms about the pointer; otherwise it is left
    /// to the scrolled window.
    pub(super) fn on_scroll(
        self: &Rc<Self>,
        state: gdk::ModifierType,
        delta: f64,
        anchor: Option<(f64, f64)>,
    ) -> glib::Propagation {
        if !state.contains(gdk::ModifierType::CONTROL_MASK) || !self.ready.get() {
            return glib::Propagation::Proceed;
        }
        let scale = self.bounds().wheeled(self.canvas.scale(), delta);
        self.set_level(Level::Scale(scale), anchor);
        glib::Propagation::Stop
    }

    /// Moves the view by one key press's worth, if the photo is larger than
    /// the view that way; true when it did.
    fn pan(&self, (across, down): (f64, f64)) -> bool {
        let Some(scroller) = self.scroller.upgrade() else {
            return false;
        };
        let (wide, tall) = self.pannable();
        let mut moved = false;
        for (adjustment, direction, scrolls) in [
            (scroller.hadjustment(), across, wide),
            (scroller.vadjustment(), down, tall),
        ] {
            if direction != 0.0 && scrolls {
                adjustment.set_value(adjustment.value() + direction * adjustment.step_increment());
                moved = true;
            }
        }
        moved
    }

    fn layout_changed(self: &Rc<Self>) {
        self.sync_controls();
        self.refine();
    }

    /// Loads more of the photo when what is on screen has become soft.
    fn refine(self: &Rc<Self>) {
        if !self.ready.get() {
            return;
        }
        let factor = self.canvas.scale_factor();
        let scale = self.canvas.scale();
        let density = zoom::density(self.texture_size(), self.canvas.picture());
        if zoom::needs_sharper(density, scale, factor) {
            self.load(zoom::next_density(scale, factor));
        }
    }

    fn load(self: &Rc<Self>, density: f64) {
        if self.loading.get().is_some_and(|loading| loading >= density) {
            return;
        }
        let size = zoom::decode_size(self.canvas.picture(), density, NATIVE_PIXELS);
        let have = self.texture_size();
        if size.0 <= have.0 && size.1 <= have.1 {
            return;
        }
        self.loading.set(Some(density));
        let state = self.clone();
        decode_photo(
            self.item().path.clone(),
            size.0,
            size.1,
            &self.token,
            move |texture| {
                state.loading.set(None);
                state
                    .canvas
                    .set_picture(Some(texture.clone()), state.canvas.picture());
                state.texture.replace(Some(texture.clone()));
                if state.has_real_size() {
                    for then in state.waiting.take() {
                        then(&texture);
                    }
                }
                state.refine();
            },
        );
    }

    fn with_real_size(self: &Rc<Self>, then: impl FnOnce(&gdk::Texture) + 'static) {
        let texture = self.texture.borrow().clone();
        match texture {
            Some(texture) if self.has_real_size() => then(&texture),
            Some(_) => {
                self.waiting.borrow_mut().push(Box::new(then));
                self.load(1.0);
            }
            None => {}
        }
    }
}

/// Opens the photos in the viewer, starting at `start`: fitted to the window,
/// then zoomed freely with the pill, the keyboard, Ctrl and the scroll wheel
/// or a pinch, and dragged around when larger than the window.
pub(in crate::native_media_widgets) fn show_photo(
    parent: &impl IsA<gtk::Widget>,
    items: Rc<Vec<PhotoItem>>,
    start: usize,
    on_action: Rc<dyn Fn(NativeMediaAction)>,
) {
    if items.is_empty() {
        return;
    }
    let start = start.min(items.len() - 1);
    let canvas = ZoomCanvas::new();
    let scroller = gtk::ScrolledWindow::builder()
        .child(&canvas)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .build();

    let pill = ZoomPill::new();
    let copy = header_button("edit-copy-symbolic", &gettext("Copy Image"));
    copy.set_sensitive(false);
    let folder = header_button("folder-open-symbolic", &gettext("Show in Folder"));
    let open = header_button(
        "adw-external-link-symbolic",
        &gettext("Open With Another App"),
    );
    let previous = step_button(
        "go-previous-symbolic",
        &gettext("Previous Photo"),
        gtk::Align::Start,
    );
    let next = step_button("go-next-symbolic", &gettext("Next Photo"), gtk::Align::End);
    let overlay = gtk::Overlay::builder().child(&scroller).build();
    if items.len() > 1 {
        overlay.add_overlay(&previous);
        overlay.add_overlay(&next);
    }
    let viewer = media_viewer(
        parent,
        &items[start].details,
        &overlay,
        Some(&pill),
        &[
            copy.clone().upcast(),
            folder.clone().upcast(),
            open.clone().upcast(),
        ],
    );
    let dialog = viewer.dialog.clone();
    keep_while_open(&dialog, items.iter().map(|item| item.path.clone()));
    let photo = Rc::new(PhotoState {
        items,
        current: Cell::new(start),
        canvas: canvas.clone(),
        scroller: scroller.downgrade(),
        pill: pill.downgrade(),
        copy: copy.downgrade(),
        previous: previous.downgrade(),
        next: next.downgrade(),
        parts: viewer.parts,
        first_decode: zoom::decode_box(viewer_size(parent), parent.scale_factor(), FIT_EDGE),
        ready: Cell::new(false),
        texture: RefCell::default(),
        loading: Cell::new(None),
        waiting: RefCell::default(),
        token: DecodeToken::default(),
    });
    pill.connect(&photo);
    canvas.connect_layout_changed({
        let photo = Rc::downgrade(&photo);
        move |_| {
            if let Some(photo) = photo.upgrade() {
                photo.layout_changed();
            }
        }
    });

    copy.connect_clicked({
        let photo = photo.clone();
        move |button| {
            let clipboard = button.clipboard();
            photo.with_real_size(move |texture| clipboard.set_texture(texture));
        }
    });
    folder.connect_clicked({
        let photo = photo.clone();
        move |button| show_in_folder(button, &photo.item().path)
    });
    open.connect_clicked({
        let (photo, dialog) = (photo.clone(), dialog.downgrade());
        move |_| {
            on_action(NativeMediaAction::Open(photo.item().path.clone()));
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        }
    });
    for (button, delta) in [(&previous, -1), (&next, 1)] {
        let photo = photo.clone();
        button.connect_clicked(move |_| photo.step(delta));
    }

    let pointer: Rc<Cell<Option<(f64, f64)>>> = Rc::default();
    let motion = gtk::EventControllerMotion::new();
    motion.connect_motion({
        let pointer = pointer.clone();
        move |_, x, y| pointer.set(Some((x, y)))
    });
    motion.connect_leave({
        let pointer = pointer.clone();
        move |_| pointer.set(None)
    });
    scroller.add_controller(motion);

    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
    wheel.connect_scroll({
        let (photo, pointer) = (photo.clone(), pointer.clone());
        move |controller, _, delta| {
            photo.on_scroll(controller.current_event_state(), delta, pointer.get())
        }
    });
    scroller.add_controller(wheel);

    let pinch = gtk::GestureZoom::new();
    let pinched_from = Rc::new(Cell::new(1.0));
    pinch.connect_begin({
        let (photo, pinched_from) = (photo.clone(), pinched_from.clone());
        move |_, _| pinched_from.set(photo.canvas.scale())
    });
    pinch.connect_scale_changed({
        let photo = photo.clone();
        move |gesture, factor| {
            photo.set_level(
                Level::Scale(pinched_from.get() * factor),
                gesture.bounding_box_center(),
            );
        }
    });
    scroller.add_controller(pinch);

    let double_click = gtk::GestureClick::new();
    double_click.connect_pressed({
        let photo = photo.clone();
        move |_, presses, x, y| {
            if presses == 2 {
                photo.toggle_fit(Some((x, y)));
            }
        }
    });
    scroller.add_controller(double_click);
    let pan = gtk::GestureDrag::new();
    let origin = Rc::new(Cell::new((0.0, 0.0)));
    {
        let (scroller, origin) = (scroller.clone(), origin.clone());
        pan.connect_drag_begin(move |_, _, _| {
            origin.set((
                scroller.hadjustment().value(),
                scroller.vadjustment().value(),
            ));
            if scroller.cursor().is_some() {
                scroller.set_cursor_from_name(Some("grabbing"));
            }
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
    {
        let photo = photo.clone();
        pan.connect_drag_end(move |_, _, _| photo.sync_controls());
    }
    scroller.add_controller(pan);

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let photo = photo.clone();
        move |controller, key, _, modifiers| {
            if let Some(command) = zoom_key(key, modifiers) {
                if !photo.ready.get() {
                    return glib::Propagation::Proceed;
                }
                match command {
                    ZoomKey::In => photo.zoom_by(true),
                    ZoomKey::Out => photo.zoom_by(false),
                    ZoomKey::Fit => photo.set_level(Level::Fit, None),
                    ZoomKey::Actual => photo.set_level(Level::Scale(zoom::ACTUAL), None),
                }
                return glib::Propagation::Stop;
            }
            // A photo larger than the view is scrolled by the arrows instead.
            if let Some(direction) = pan_key(key, modifiers)
                && photo.pan(direction)
            {
                return glib::Propagation::Stop;
            }
            let rtl = controller
                .widget()
                .is_some_and(|widget| widget.direction() == gtk::TextDirection::Rtl);
            match key_step(key, modifiers, rtl) {
                Some(delta) => {
                    photo.step(delta);
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        }
    });
    dialog.add_controller(keys);
    #[cfg(test)]
    OPEN_PHOTO.with(|open| open.replace(Some(photo.clone())));
    photo.show(start);
    dialog.present(Some(parent));
}

#[cfg(test)]
thread_local! {
    pub(super) static OPEN_PHOTO: RefCell<Option<Rc<PhotoState>>> = const { RefCell::new(None) };
}
