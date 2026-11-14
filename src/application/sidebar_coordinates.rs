use gtk4 as gtk;
use std::cell::{Cell, RefCell};
use std::sync::OnceLock;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

#[derive(Clone)]
struct VisibleChat {
    id: String,
    y: f64,
}

#[derive(Clone)]
struct Geometry {
    chats: Vec<VisibleChat>,
    proxy: f64,
}

struct Restore {
    id: String,
    position: u32,
    y: f64,
    proxy: f64,
    last_input: f64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum RestorePlan {
    Anchor(u32),
    NoGeometry,
    NoSurvivor,
}

#[cfg(test)]
type SnapshotObserver = Box<dyn Fn(&gtk::ListView)>;

mod imp {
    use super::*;

    pub struct SidebarCoordinates {
        pub child: RefCell<Option<gtk::ListView>>,
        pub proxy: RefCell<Option<gtk::Adjustment>>,
        pub real: RefCell<Option<gtk::Adjustment>>,
        pub horizontal: RefCell<Option<gtk::Adjustment>>,
        pub horizontal_policy: Cell<gtk::ScrollablePolicy>,
        pub vertical_policy: Cell<gtk::ScrollablePolicy>,
        pub proxy_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub real_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub origin: Cell<f64>,
        pub guard: Cell<bool>,
        pub expected_proxy: Cell<Option<f64>>,
        pub proxy_intent: RefCell<Option<(gtk::Adjustment, f64)>>,
        pub draining_intent: Cell<bool>,
        pub dirty: Cell<bool>,
        pub reconcile_scheduled: Cell<bool>,
        pub allocating: Cell<bool>,
        pub pending: Cell<bool>,
        pub(super) geometry: RefCell<Option<Geometry>>,
        pub(super) restore: RefCell<Option<Restore>>,
        pub(super) restore_generation: Cell<u64>,
        #[cfg(test)]
        pub(super) snapshot_observer: RefCell<Option<SnapshotObserver>>,
        #[cfg(test)]
        pub(super) delayed_attempts: Cell<u32>,
        #[cfg(test)]
        pub(super) observed_attempts: Cell<u32>,
    }

    impl Default for SidebarCoordinates {
        fn default() -> Self {
            Self {
                child: RefCell::new(None),
                proxy: RefCell::new(None),
                real: RefCell::new(None),
                horizontal: RefCell::new(None),
                horizontal_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                vertical_policy: Cell::new(gtk::ScrollablePolicy::Minimum),
                proxy_handler: RefCell::new(None),
                real_handlers: RefCell::new(Vec::new()),
                origin: Cell::new(0.0),
                guard: Cell::new(false),
                expected_proxy: Cell::new(None),
                proxy_intent: RefCell::new(None),
                draining_intent: Cell::new(false),
                dirty: Cell::new(false),
                reconcile_scheduled: Cell::new(false),
                allocating: Cell::new(false),
                pending: Cell::new(false),
                geometry: RefCell::new(None),
                restore: RefCell::new(None),
                restore_generation: Cell::new(0),
                #[cfg(test)]
                snapshot_observer: RefCell::new(None),
                #[cfg(test)]
                delayed_attempts: Cell::new(0),
                #[cfg(test)]
                observed_attempts: Cell::new(0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SidebarCoordinates {
        const NAME: &'static str = "ZapTideSidebarCoordinates";
        type Type = super::SidebarCoordinates;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);
    }

    impl ObjectImpl for SidebarCoordinates {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                [
                    "hadjustment",
                    "vadjustment",
                    "hscroll-policy",
                    "vscroll-policy",
                ]
                .into_iter()
                .map(glib::ParamSpecOverride::for_interface::<gtk::Scrollable>)
                .collect()
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "hadjustment" => {
                    let adjustment = value.get::<Option<gtk::Adjustment>>().unwrap();
                    self.horizontal.replace(adjustment.clone());
                    let child = self.child.borrow().clone();
                    if let Some(child) = child {
                        child.set_hadjustment(adjustment.as_ref());
                    }
                }
                "vadjustment" => {
                    self.obj().cancel_restore();
                    self.geometry.take();
                    let old = self.proxy.borrow().clone();
                    let handler = self.proxy_handler.borrow_mut().take();
                    if let (Some(old), Some(handler)) = (old, handler) {
                        old.disconnect(handler);
                    }
                    let adjustment = value.get::<Option<gtk::Adjustment>>().unwrap();
                    if let Some(adjustment) = adjustment.as_ref() {
                        let weak = self.obj().downgrade();
                        let handler = adjustment.connect_value_changed(move |proxy| {
                            if let Some(widget) = weak.upgrade() {
                                widget.forward(proxy);
                            }
                        });
                        self.proxy_handler.replace(Some(handler));
                    }
                    self.proxy.replace(adjustment);
                    self.obj().publish_ranges(false);
                }
                "hscroll-policy" => {
                    let policy = value.get().unwrap();
                    self.horizontal_policy.set(policy);
                    let child = self.child.borrow().clone();
                    if let Some(child) = child {
                        child.set_hscroll_policy(policy);
                    }
                }
                "vscroll-policy" => {
                    let policy = value.get().unwrap();
                    self.vertical_policy.set(policy);
                    let child = self.child.borrow().clone();
                    if let Some(child) = child {
                        child.set_vscroll_policy(policy);
                    }
                }
                _ => unreachable!(),
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "hadjustment" => self.horizontal.borrow().to_value(),
                "vadjustment" => self.proxy.borrow().to_value(),
                "hscroll-policy" => self.horizontal_policy.get().to_value(),
                "vscroll-policy" => self.vertical_policy.get().to_value(),
                _ => unreachable!(),
            }
        }

        fn dispose(&self) {
            self.obj().cancel_restore();
            let proxy = self.proxy.borrow_mut().take();
            let handler = self.proxy_handler.borrow_mut().take();
            if let (Some(proxy), Some(handler)) = (proxy, handler) {
                proxy.disconnect(handler);
            }
            let real = self.real.borrow_mut().take();
            let handlers = self.real_handlers.take();
            if let Some(real) = real {
                for handler in handlers {
                    real.disconnect(handler);
                }
            }
            let child = self.child.borrow_mut().take();
            if let Some(child) = child {
                child.unparent();
            }
            self.horizontal.take();
            self.proxy_intent.take();
        }
    }

    impl WidgetImpl for SidebarCoordinates {
        fn unmap(&self) {
            self.obj().cancel_restore();
            self.geometry.take();
            self.parent_unmap();
        }

        fn request_mode(&self) -> gtk::SizeRequestMode {
            let child = self.child.borrow().clone();
            child
                .as_ref()
                .map_or(gtk::SizeRequestMode::ConstantSize, |child| {
                    child.request_mode()
                })
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let child = self.child.borrow().clone();
            child
                .as_ref()
                .map_or((0, 0, -1, -1), |child| child.measure(orientation, for_size))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.allocating.set(true);
            let child = self.child.borrow().clone();
            if let Some(child) = child {
                child.allocate(width, height, baseline, None);
                for _ in 0..3 {
                    if !self.obj().restore_after_allocation(&child) {
                        break;
                    }
                    child.allocate(width, height, baseline, None);
                }
                if self.pending.get() {
                    log::warn!("sidebar anchor could not be materialized during allocation");
                    self.obj().cancel_restore();
                    self.obj().publish_ranges(true);
                }
            }
            self.allocating.set(false);
            self.obj().publish_ranges(false);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let child = self.child.borrow().clone();
            if let Some(child) = child {
                self.obj().capture_geometry(&child);
                #[cfg(test)]
                if let Some(observer) = self.snapshot_observer.borrow().as_ref() {
                    observer(&child);
                }
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }
    impl ScrollableImpl for SidebarCoordinates {}
}

glib::wrapper! {
    pub struct SidebarCoordinates(ObjectSubclass<imp::SidebarCoordinates>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

impl SidebarCoordinates {
    pub(super) fn at_top(&self) -> bool {
        let imp = self.imp();
        if imp.pending.get() {
            let proxy = imp.proxy.borrow().clone();
            return proxy.is_none_or(|proxy| proxy.value() <= proxy.lower());
        }
        imp.real
            .borrow()
            .as_ref()
            .is_none_or(|real| real.value() <= real.lower())
    }

    #[cfg(test)]
    pub(super) fn observe_snapshot(&self, observer: impl Fn(&gtk::ListView) + 'static) {
        self.imp()
            .snapshot_observer
            .replace(Some(Box::new(observer)));
    }

    #[cfg(test)]
    pub(super) fn restore_pending(&self) -> bool {
        self.imp().pending.get()
    }

    #[cfg(test)]
    pub(super) fn forget_geometry(&self) {
        self.imp().geometry.take();
    }

    #[cfg(test)]
    pub(super) fn attempt_restore_before_layout(&self, child: &gtk::ListView) {
        self.restore_after_allocation(child);
    }

    #[cfg(test)]
    pub(super) fn delay_materialization(&self, attempts: u32) {
        self.imp().delayed_attempts.set(attempts);
        self.imp().observed_attempts.set(0);
    }

    #[cfg(test)]
    pub(super) fn materialization_attempts(&self) -> u32 {
        self.imp().observed_attempts.get()
    }

    fn capture_geometry(&self, child: &gtk::ListView) {
        let imp = self.imp();
        if imp.pending.get() {
            return;
        }
        let Some(proxy) = imp.proxy.borrow().clone() else {
            return;
        };
        let mut chats = Vec::new();
        let mut item = child.first_child();
        while let Some(current) = item {
            if current.is_mapped()
                && let Some(root) = current.first_child()
                && let Some(bounds) = root.compute_bounds(child)
                && bounds.y() < child.height() as f32
                && bounds.y() + bounds.height() > 0.0
            {
                chats.push(VisibleChat {
                    id: root.widget_name().to_string(),
                    y: f64::from(bounds.y()),
                });
            }
            item = current.next_sibling();
        }
        chats.sort_by(|a, b| (a.y < 0.0).cmp(&(b.y < 0.0)).then(a.y.total_cmp(&b.y)));
        imp.geometry.replace(Some(Geometry {
            chats,
            proxy: proxy.value(),
        }));
    }

    pub(super) fn begin_restore(&self, ids: &[String]) -> RestorePlan {
        let imp = self.imp();
        let proxy = imp.proxy.borrow().clone();
        let geometry = imp.geometry.borrow().clone();
        let (Some(proxy), Some(geometry)) = (proxy, geometry) else {
            self.cancel_restore();
            return RestorePlan::NoGeometry;
        };
        let chat = geometry
            .chats
            .iter()
            .find_map(|chat| ids.iter().position(|id| id == &chat.id).map(|i| (chat, i)));
        let Some(chat) = chat else {
            self.cancel_restore();
            return RestorePlan::NoSurvivor;
        };
        imp.restore.replace(Some(Restore {
            id: chat.0.id.clone(),
            position: chat.1 as u32,
            y: chat.0.y,
            proxy: geometry.proxy,
            last_input: proxy.value(),
        }));
        imp.restore_generation
            .set(imp.restore_generation.get().wrapping_add(1));
        imp.pending.set(true);
        RestorePlan::Anchor(chat.1 as u32)
    }

    pub(super) fn reset_account_geometry(&self) {
        self.cancel_restore();
        self.imp().geometry.take();
    }

    pub(super) fn cancel_restore(&self) {
        let imp = self.imp();
        let restore = imp.restore.take();
        imp.pending.set(false);
        if restore.is_some() {
            imp.restore_generation
                .set(imp.restore_generation.get().wrapping_add(1));
            let real = imp.real.borrow().clone();
            let proxy = imp.proxy.borrow().clone();
            if let (Some(real), Some(proxy)) = (real, proxy) {
                imp.origin.set(real.value() - proxy.value());
            }
        }
    }

    fn restore_after_allocation(&self, child: &gtk::ListView) -> bool {
        let imp = self.imp();
        let generation = imp.restore_generation.get();
        let Some((id, y, original_proxy)) = imp
            .restore
            .borrow()
            .as_ref()
            .map(|restore| (restore.id.clone(), restore.y, restore.proxy))
        else {
            return false;
        };
        #[cfg(test)]
        {
            imp.observed_attempts.set(imp.observed_attempts.get() + 1);
            let remaining = imp.delayed_attempts.get();
            if remaining > 0 {
                assert!(child.is_mapped() && child.width() > 0 && child.height() > 0);
                imp.delayed_attempts.set(remaining - 1);
                child.queue_allocate();
                return true;
            }
        }
        let (Some(proxy), Some(real)) = (imp.proxy.borrow().clone(), imp.real.borrow().clone())
        else {
            self.cancel_restore();
            return false;
        };
        let mut item = child.first_child();
        while let Some(current) = item {
            if current.is_mapped()
                && let Some(root) = current.first_child()
                && root.widget_name() == id
                && let Some(bounds) = root.compute_bounds(child)
            {
                let desired_y = y - (proxy.value() - original_proxy);
                let value = (real.value() + f64::from(bounds.y()) - desired_y).clamp(
                    real.lower(),
                    (real.upper() - real.page_size()).max(real.lower()),
                );
                real.set_value(value);
                if imp.restore_generation.get() != generation {
                    return false;
                }
                imp.origin.set(real.value() - proxy.value());
                self.cancel_restore();
                self.publish_ranges(true);
                return true;
            }
            item = current.next_sibling();
        }
        let geometry = imp.geometry.borrow().clone();
        let model = child.model();
        let survivor = geometry.and_then(|geometry| {
            let model = model.as_ref()?;
            geometry.chats.into_iter().find_map(|chat| {
                (0..model.n_items()).find_map(|position| {
                    let item = model
                        .item(position)?
                        .downcast::<glib::BoxedAnyObject>()
                        .ok()?;
                    let row = item.try_borrow::<super::ChatRow>().ok()?;
                    (row.id == chat.id).then_some((chat.clone(), position))
                })
            })
        });
        if let Some((chat, position)) = survivor {
            if let Some(restore) = imp.restore.borrow_mut().as_mut() {
                restore.id = chat.id;
                restore.y = chat.y;
                restore.position = position;
            }
            child.scroll_to(position, gtk::ListScrollFlags::NONE, None);
            child.queue_allocate();
            return true;
        }
        self.cancel_restore();
        if child.model().is_some_and(|model| model.n_items() > 0) {
            child.scroll_to(0, gtk::ListScrollFlags::NONE, None);
        }
        self.publish_ranges(false);
        true
    }

    pub(super) fn new(child: &gtk::ListView) -> Self {
        let widget: Self = glib::Object::new();
        let imp = widget.imp();
        child.set_parent(&widget);
        imp.child.replace(Some(child.clone()));
        let real = gtk::Adjustment::new(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        child.set_vadjustment(Some(&real));
        for changed in [false, true] {
            let weak = widget.downgrade();
            let callback = move |_: &gtk::Adjustment| {
                if let Some(widget) = weak.upgrade() {
                    widget.publish_ranges(false);
                }
            };
            let handler = if changed {
                real.connect_value_changed(callback)
            } else {
                real.connect_changed(callback)
            };
            imp.real_handlers.borrow_mut().push(handler);
        }
        imp.real.replace(Some(real));
        widget
    }

    fn publish_ranges(&self, preserve_proxy: bool) {
        let imp = self.imp();
        if imp.guard.get() {
            imp.dirty.set(true);
            return;
        }
        if imp.allocating.get() || (imp.pending.get() && !preserve_proxy) {
            return;
        }
        let mut preserve = preserve_proxy;
        for _ in 0..8 {
            imp.dirty.set(false);
            let proxy = imp.proxy.borrow().clone();
            let real = imp.real.borrow().clone();
            let (Some(proxy), Some(real)) = (proxy, real) else {
                return;
            };
            let origin = imp.origin.get();
            let target = if preserve {
                proxy.value()
            } else {
                real.value() - origin
            };
            imp.expected_proxy.set(Some(target));
            imp.guard.set(true);
            proxy.configure(
                target,
                real.lower() - origin,
                real.upper() - origin,
                real.step_increment(),
                real.page_increment(),
                real.page_size(),
            );
            imp.guard.set(false);
            imp.expected_proxy.set(None);
            self.drain_proxy_intents();
            if !imp.dirty.get() {
                return;
            }
            preserve = false;
        }
        if !imp.reconcile_scheduled.replace(true) {
            let weak = self.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(widget) = weak.upgrade() {
                    widget.imp().reconcile_scheduled.set(false);
                    widget.drain_proxy_intents();
                    widget.publish_ranges(false);
                }
            });
        }
    }

    fn forward(&self, proxy: &gtk::Adjustment) {
        let imp = self.imp();
        if imp.guard.get() {
            if imp.expected_proxy.get() != Some(proxy.value())
                && imp.proxy.borrow().as_ref() == Some(proxy)
            {
                imp.proxy_intent
                    .replace(Some((proxy.clone(), proxy.value())));
            }
            return;
        }
        if imp.pending.get() {
            let intent = {
                let mut restore = imp.restore.borrow_mut();
                restore.as_mut().map(|restore| {
                    let delta = proxy.value() - restore.last_input;
                    restore.last_input = proxy.value();
                    (delta, restore.position)
                })
            };
            let real = imp.real.borrow().clone();
            let generation = imp.restore_generation.get();
            if let (Some((delta, position)), Some(real)) = (intent, real) {
                real.set_value(real.value() + delta);
                if imp.restore_generation.get() == generation {
                    let child = imp.child.borrow().clone();
                    if let Some(child) = child
                        && child.is_mapped()
                        && child
                            .model()
                            .is_some_and(|model| position < model.n_items())
                    {
                        child.scroll_to(position, gtk::ListScrollFlags::NONE, None);
                    }
                }
            }
            return;
        }
        if imp.proxy.borrow().as_ref() == Some(proxy) {
            imp.proxy_intent.take();
        }
        self.forward_value(proxy, proxy.value());
    }

    fn forward_value(&self, proxy: &gtk::Adjustment, before: f64) {
        let imp = self.imp();
        let Some(real) = imp.real.borrow().clone() else {
            return;
        };
        let origin = imp.origin.get();
        let requested = before + origin;
        imp.expected_proxy.set(Some(proxy.value()));
        imp.guard.set(true);
        real.set_value(requested);
        let accepted = real.value();
        imp.guard.set(false);
        imp.expected_proxy.set(None);
        if imp.proxy.borrow().as_ref() == Some(proxy) && proxy.value() != before {
            imp.proxy_intent
                .replace(Some((proxy.clone(), proxy.value())));
        }
        self.drain_proxy_intents();
        let lower = real.lower() - origin;
        let upper = (real.upper() - origin - real.page_size()).max(lower);
        let current = imp.proxy.borrow().clone();
        let keep = current.as_ref() == Some(proxy)
            && accepted == requested
            && proxy.value() == before
            && before >= lower
            && before <= upper;
        self.publish_ranges(keep);
    }

    fn drain_proxy_intents(&self) {
        let imp = self.imp();
        if imp.guard.get() || imp.draining_intent.replace(true) {
            return;
        }
        for _ in 0..8 {
            let intent = imp.proxy_intent.borrow_mut().take();
            let Some((proxy, value)) = intent else {
                break;
            };
            if !imp.pending.get() && imp.proxy.borrow().as_ref() == Some(&proxy) {
                self.forward_value(&proxy, value);
            }
        }
        imp.draining_intent.set(false);
        if imp.proxy_intent.borrow().is_some() && !imp.reconcile_scheduled.replace(true) {
            let weak = self.downgrade();
            glib::idle_add_local_once(move || {
                if let Some(widget) = weak.upgrade() {
                    widget.imp().reconcile_scheduled.set(false);
                    widget.drain_proxy_intents();
                    widget.publish_ranges(false);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn unmap_cancels_pending_restore_without_losing_scroll() {
        let (bridge, _child, proxy, real) = fixture();
        let window = gtk::Window::new();
        window.set_child(Some(&bridge));
        window.present();
        let context = glib::MainContext::default();
        for _ in 0..20 {
            while context.pending() {
                context.iteration(false);
            }
            if bridge.is_mapped() {
                break;
            }
        }
        assert!(bridge.is_mapped());
        real.configure(100.0, 0.0, 2000.0, 1.0, 100.0, 100.0);
        bridge.imp().restore.replace(Some(Restore {
            id: "synthetic".into(),
            position: 0,
            y: 0.0,
            proxy: proxy.value(),
            last_input: proxy.value(),
        }));
        bridge.imp().pending.set(true);
        real.set_value(700.0);
        window.set_visible(false);
        assert!(!bridge.imp().pending.get());
        assert_eq!(real.value(), proxy.value() + bridge.imp().origin.get());
        proxy.set_value(proxy.value() + 10.0);
        assert_eq!(real.value(), 710.0);
        window.close();
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn pending_input_can_dispose_wrapper_synchronously() {
        let (bridge, child, proxy, real) = fixture();
        bridge.imp().restore.replace(Some(Restore {
            id: "synthetic".into(),
            position: 0,
            y: 0.0,
            proxy: 100.0,
            last_input: 100.0,
        }));
        bridge.imp().pending.set(true);
        let weak = bridge.downgrade();
        real.connect_value_changed(move |_| {
            if let Some(bridge) = weak.upgrade() {
                unsafe { bridge.run_dispose() };
            }
        });
        proxy.set_value(150.0);
        assert!(bridge.imp().child.borrow().is_none());
        assert!(child.parent().is_none());
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn replacement_does_not_publish_cancel_to_old_proxy() {
        let (bridge, _child, old, real) = fixture();
        bridge.imp().restore.replace(Some(Restore {
            id: "synthetic".into(),
            position: 0,
            y: 0.0,
            proxy: 100.0,
            last_input: 100.0,
        }));
        bridge.imp().pending.set(true);
        real.set_value(700.0);
        let changes = std::rc::Rc::new(Cell::new(0));
        let observed = changes.clone();
        old.connect_value_changed(move |_| observed.set(observed.get() + 1));
        let replacement = gtk::Adjustment::new(0.0, 0.0, 2000.0, 1.0, 100.0, 100.0);
        bridge.set_vadjustment(Some(&replacement));
        assert_eq!(changes.get(), 0);
        assert!(!bridge.imp().pending.get());
        assert_eq!(
            real.value(),
            replacement.value() + bridge.imp().origin.get()
        );
        replacement.set_value(replacement.value() + 10.0);
        assert_eq!(real.value(), 710.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn chained_restore_without_visible_survivors_cancels_pending() {
        let (bridge, _child, proxy, real) = fixture();
        bridge.imp().geometry.replace(Some(Geometry {
            chats: vec![VisibleChat {
                id: "old".into(),
                y: 10.0,
            }],
            proxy: proxy.value(),
        }));
        assert_eq!(
            bridge.begin_restore(&["old".into()]),
            RestorePlan::Anchor(0)
        );
        real.set_value(700.0);
        assert_eq!(
            bridge.begin_restore(&["new".into()]),
            RestorePlan::NoSurvivor
        );
        assert!(!bridge.imp().pending.get());
        assert_eq!(real.value(), proxy.value() + bridge.imp().origin.get());
        proxy.set_value(proxy.value() + 10.0);
        assert_eq!(real.value(), 710.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn cancel_after_pending_input_preserves_coordinates() {
        let (bridge, _child, proxy, real) = fixture();
        bridge.imp().origin.set(100.0);
        bridge.imp().restore.replace(Some(Restore {
            id: "synthetic".into(),
            position: 0,
            y: 0.0,
            proxy: 100.0,
            last_input: 100.0,
        }));
        bridge.imp().pending.set(true);
        real.set_value(700.0);
        proxy.set_value(150.0);
        let accepted = real.value();
        bridge.cancel_restore();
        assert_eq!(real.value(), proxy.value() + bridge.imp().origin.get());
        proxy.set_value(160.0);
        assert_eq!(real.value(), accepted + 10.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn top_uses_presented_bounds_while_restore_is_pending() {
        let (bridge, _child, proxy, real) = fixture();
        bridge.imp().origin.set(100.0);
        bridge.publish_ranges(false);
        proxy.set_value(proxy.lower());
        assert!(bridge.at_top());
        bridge.imp().pending.set(true);
        real.set_value(300.0);
        assert!(bridge.at_top());
        bridge.imp().pending.set(false);
        real.set_value(100.0);
        assert_eq!(proxy.value(), 0.0);
        assert!(!bridge.at_top());
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn scrolled_window_owns_wrapper_and_preserves_list_selection() {
        gtk::init().expect("display required for sidebar coordinate test");
        let selection = gtk::SingleSelection::new(None::<gtk::gio::ListModel>);
        let child = gtk::ListView::new(Some(selection.clone()), None::<gtk::ListItemFactory>);
        let bridge = SidebarCoordinates::new(&child);
        let weak_bridge = bridge.downgrade();
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_child(Some(&bridge));
        drop(bridge);
        let bridge = weak_bridge.upgrade().expect("parent retains wrapper");
        assert_eq!(child.parent().as_ref(), Some(bridge.upcast_ref()));
        assert_eq!(child.model().as_ref(), Some(selection.upcast_ref()));
        assert_eq!(bridge.vadjustment(), Some(scrolled.vadjustment()));
        assert_eq!(child.hadjustment(), bridge.hadjustment());
        scrolled.set_child(None::<&gtk::Widget>);
        drop(bridge);
        assert!(weak_bridge.upgrade().is_none());
        assert!(child.parent().is_none());
    }

    fn fixture() -> (
        SidebarCoordinates,
        gtk::ListView,
        gtk::Adjustment,
        gtk::Adjustment,
    ) {
        gtk::init().expect("display required for sidebar coordinate test");
        let child = gtk::ListView::new(None::<gtk::SelectionModel>, None::<gtk::ListItemFactory>);
        let bridge = SidebarCoordinates::new(&child);
        let proxy = gtk::Adjustment::new(0.0, 0.0, 2000.0, 1.0, 100.0, 100.0);
        bridge.set_vadjustment(Some(&proxy));
        let real = bridge.imp().real.borrow().clone().unwrap();
        real.configure(100.0, 0.0, 2000.0, 1.0, 100.0, 100.0);
        (bridge, child, proxy, real)
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn reentrant_real_write_during_publication_converges() {
        let (bridge, _child, proxy, real) = fixture();
        let once = std::rc::Rc::new(Cell::new(false));
        let once_in_handler = once.clone();
        let real_in_handler = real.clone();
        proxy.connect_changed(move |_| {
            if !once_in_handler.replace(true) {
                real_in_handler.set_value(350.0);
            }
        });
        bridge.imp().pending.set(true);
        real.configure(200.0, 0.0, 2100.0, 1.0, 100.0, 100.0);
        bridge.imp().pending.set(false);
        bridge.publish_ranges(false);
        assert!(once.get());
        assert_eq!(real.value(), 350.0);
        assert_eq!(proxy.value(), 350.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn reentrant_proxy_replacement_publishes_only_to_current_adjustment() {
        let (bridge, _child, old, real) = fixture();
        let replacement = gtk::Adjustment::new(0.0, 0.0, 2000.0, 1.0, 100.0, 100.0);
        let weak = bridge.downgrade();
        let new_proxy = replacement.clone();
        let once = std::rc::Rc::new(Cell::new(false));
        let callback_once = once.clone();
        old.connect_value_changed(move |_| {
            if !callback_once.replace(true) {
                weak.upgrade().unwrap().set_vadjustment(Some(&new_proxy));
            }
        });
        real.set_value(250.0);
        assert!(once.get());
        assert_eq!(bridge.vadjustment().as_ref(), Some(&replacement));
        assert_eq!(replacement.value(), 250.0);
        old.set_value(300.0);
        assert_eq!(real.value(), 250.0);
        real.set_value(275.0);
        assert_eq!(replacement.value(), 275.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn replacement_during_forward_mirrors_the_accepted_real_value() {
        let (bridge, _child, old, real) = fixture();
        let replacement = gtk::Adjustment::new(0.0, 0.0, 2000.0, 1.0, 100.0, 100.0);
        let weak = bridge.downgrade();
        let new_proxy = replacement.clone();
        let once = std::rc::Rc::new(Cell::new(false));
        let callback_once = once.clone();
        real.connect_value_changed(move |_| {
            if !callback_once.replace(true) {
                weak.upgrade().unwrap().set_vadjustment(Some(&new_proxy));
            }
        });
        old.set_value(250.0);
        assert!(once.get());
        assert_eq!(real.value(), 250.0);
        assert_eq!(replacement.value(), 250.0);
        old.set_value(300.0);
        assert_eq!(real.value(), 250.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn nested_proxy_write_during_forward_is_not_lost() {
        let (_bridge, _child, proxy, real) = fixture();
        let weak = proxy.downgrade();
        let once = std::rc::Rc::new(Cell::new(false));
        let callback_once = once.clone();
        real.connect_value_changed(move |_| {
            if !callback_once.replace(true) {
                weak.upgrade().unwrap().set_value(350.0);
            }
        });
        proxy.set_value(200.0);
        assert!(once.get());
        assert_eq!(proxy.value(), 350.0);
        assert_eq!(real.value(), 350.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn nested_proxy_write_during_range_publication_is_not_lost() {
        let (bridge, _child, proxy, real) = fixture();
        let weak = proxy.downgrade();
        let once = std::rc::Rc::new(Cell::new(false));
        let callback_once = once.clone();
        proxy.connect_changed(move |_| {
            if !callback_once.replace(true) {
                weak.upgrade().unwrap().set_value(350.0);
            }
        });
        bridge.imp().pending.set(true);
        real.configure(200.0, 0.0, 2100.0, 1.0, 100.0, 100.0);
        bridge.imp().pending.set(false);
        bridge.publish_ranges(false);
        assert!(once.get());
        assert_eq!(proxy.value(), 350.0);
        assert_eq!(real.value(), 350.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn nested_proxy_write_during_value_publication_is_not_lost() {
        let (bridge, _child, proxy, real) = fixture();
        let weak = proxy.downgrade();
        let once = std::rc::Rc::new(Cell::new(false));
        let callback_once = once.clone();
        proxy.connect_value_changed(move |_| {
            if !callback_once.replace(true) {
                weak.upgrade().unwrap().set_value(350.0);
            }
        });
        bridge.imp().pending.set(true);
        real.set_value(200.0);
        bridge.imp().pending.set(false);
        bridge.publish_ranges(false);
        assert!(once.get());
        assert_eq!(proxy.value(), 350.0);
        assert_eq!(real.value(), 350.0);
        assert_eq!(proxy.lower(), real.lower());
        assert_eq!(proxy.upper(), real.upper());
        assert_eq!(proxy.page_size(), real.page_size());
    }

    fn deferred_fixture() -> (SidebarCoordinates, gtk::Adjustment, gtk::Adjustment) {
        let (bridge, _child, proxy, real) = fixture();
        let remaining = std::rc::Rc::new(Cell::new(20));
        let callback_remaining = remaining.clone();
        let weak_real = real.downgrade();
        proxy.connect_changed(move |proxy| {
            let count = callback_remaining.get();
            if count > 0 {
                callback_remaining.set(count - 1);
                let real = weak_real.upgrade().unwrap();
                real.set_upper(real.upper() + 1.0);
                proxy.set_value(proxy.value() + 1.0);
            }
        });
        bridge.imp().pending.set(true);
        real.set_upper(real.upper() + 1.0);
        bridge.imp().pending.set(false);
        bridge.publish_ranges(false);
        assert!(bridge.imp().reconcile_scheduled.get());
        assert!(bridge.imp().proxy_intent.borrow().is_some());
        assert!(remaining.get() <= 12);
        remaining.set(0);
        (bridge, proxy, real)
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn newer_input_precedes_deferred_intent() {
        let (bridge, proxy, real) = deferred_fixture();
        proxy.set_value(500.0);
        let context = glib::MainContext::default();
        for _ in 0..10 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        assert!(!bridge.imp().reconcile_scheduled.get());
        assert_eq!(proxy.value(), 500.0);
        assert_eq!(real.value(), 500.0);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn older_idle_does_not_cancel_new_restore() {
        let (bridge, proxy, _real) = deferred_fixture();
        bridge.imp().geometry.replace(Some(Geometry {
            chats: vec![VisibleChat {
                id: "synthetic".into(),
                y: 10.0,
            }],
            proxy: proxy.value(),
        }));
        assert_eq!(
            bridge.begin_restore(&["synthetic".into()]),
            RestorePlan::Anchor(0)
        );
        let generation = bridge.imp().restore_generation.get();
        let context = glib::MainContext::default();
        for _ in 0..10 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        assert!(!bridge.imp().reconcile_scheduled.get());
        assert!(bridge.restore_pending());
        assert_eq!(bridge.imp().restore_generation.get(), generation);
        assert_eq!(
            bridge.imp().restore.borrow().as_ref().unwrap().id,
            "synthetic"
        );
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn disposing_wrapper_cancels_deferred_reconciliation() {
        let (bridge, proxy, real) = deferred_fixture();
        let weak = bridge.downgrade();
        let original = real.value();
        drop(bridge);
        assert!(weak.upgrade().is_none());
        proxy.set_value(500.0);
        let context = glib::MainContext::default();
        for _ in 0..10 {
            if !context.pending() {
                break;
            }
            context.iteration(false);
        }
        assert_eq!(real.value(), original);
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn horizontal_notification_observes_new_wrapper_adjustment() {
        let (bridge, child, _proxy, _real) = fixture();
        let adjustment = gtk::Adjustment::new(0.0, 0.0, 100.0, 1.0, 10.0, 10.0);
        let observed = std::rc::Rc::new(RefCell::new(Vec::new()));
        let output = observed.clone();
        let weak = bridge.downgrade();
        child.connect_notify_local(Some("hadjustment"), move |_, _| {
            output
                .borrow_mut()
                .push(weak.upgrade().unwrap().hadjustment());
        });
        bridge.set_hadjustment(Some(&adjustment));
        assert_eq!(observed.borrow().as_slice(), &[Some(adjustment)]);
        bridge.set_hadjustment(None::<&gtk::Adjustment>);
        assert_eq!(observed.borrow().last(), Some(&None));
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn child_notification_can_dispose_wrapper_synchronously() {
        let (bridge, child, _proxy, _real) = fixture();
        let weak = bridge.downgrade();
        child.connect_notify_local(Some("hadjustment"), move |_, _| {
            if let Some(bridge) = weak.upgrade() {
                // The test deliberately exercises synchronous GObject disposal.
                unsafe { bridge.run_dispose() };
            }
        });
        let adjustment = gtk::Adjustment::new(0.0, 0.0, 100.0, 1.0, 10.0, 10.0);
        bridge.set_hadjustment(Some(&adjustment));
        assert!(bridge.imp().child.borrow().is_none());
    }

    #[test]
    #[ignore = "requires a display and a separate GTK process"]
    fn adjustment_forwarding_preserves_exact_proxy_and_mirrors_real_writes() {
        gtk::init().expect("display required for sidebar coordinate test");
        let child = gtk::ListView::new(None::<gtk::SelectionModel>, None::<gtk::ListItemFactory>);
        let bridge = SidebarCoordinates::new(&child);
        let proxy = gtk::Adjustment::new(0.0, 0.0, 20000.0, 1.0, 100.0, 1046.0);
        bridge.set_vadjustment(Some(&proxy));
        let real = bridge.imp().real.borrow().clone().unwrap();
        real.configure(9300.0, 0.0, 20000.0, 1.0, 100.0, 1046.0);
        bridge.imp().origin.set(9300.0);
        bridge.publish_ranges(false);
        let value = f64::from_bits(0x40afb96eefede031);
        proxy.set_value(value);
        assert_eq!(proxy.value().to_bits(), value.to_bits());
        assert_eq!(real.value(), value + 9300.0);
        real.set_value(10000.0);
        assert_eq!(proxy.value(), 700.0);

        bridge.imp().pending.set(true);
        proxy.set_value(710.0);
        assert_eq!(real.value(), 10000.0);
        real.set_value(10010.0);
        assert_eq!(proxy.value(), 710.0);
        bridge.imp().pending.set(false);
        bridge.publish_ranges(false);
        assert_eq!(proxy.value(), 710.0);

        let replacement = gtk::Adjustment::new(0.0, 0.0, 20000.0, 1.0, 100.0, 1046.0);
        bridge.set_vadjustment(Some(&replacement));
        let real_before = real.value();
        proxy.set_value(720.0);
        assert_eq!(real.value(), real_before);
        real.set_value(10020.0);
        assert_eq!(replacement.value(), 720.0);

        let weak_bridge = bridge.downgrade();
        let weak_child = child.downgrade();
        drop(bridge);
        drop(child);
        assert!(weak_bridge.upgrade().is_none());
        assert!(weak_child.upgrade().is_none());
        real.set_value(10030.0);
        replacement.set_value(730.0);
    }
}
