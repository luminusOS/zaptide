use super::*;
use std::{
    cell::Cell,
    io::Write,
    rc::Rc,
    time::{Duration, Instant},
};

fn event(phase: &str, details: serde_json::Value) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let hwm = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))
        .unwrap();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::var("ZMD_EVENTS").unwrap())
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({"phase": phase, "status": "ok", "vmhwm": hwm, "details":details})
    )
    .unwrap();
}

fn settle() {
    let context = gtk::glib::MainContext::default();
    let until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn snapshot(window: &gtk::Window, name: &str) -> usize {
    let paintable = gtk::WidgetPaintable::new(Some(window));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(window.width()),
        f64::from(window.height()),
    );
    let node = snapshot.to_node().expect("window has no rendered node");
    let texture = window
        .native()
        .unwrap()
        .renderer()
        .unwrap()
        .render_texture(&node, None);
    let (width, height) = (texture.width() as usize, texture.height() as usize);
    assert!(width > 0 && height > 0);
    let mut pixels = vec![0; width * height * 4];
    texture.download(&mut pixels, width * 4);
    let first = &pixels[0..4];
    let different = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel.as_slice() != first)
        .count();
    assert!(
        different > 100,
        "rendered texture has no meaningful pixel variation"
    );
    let path = std::path::PathBuf::from(std::env::var("ZMD_EVENTS").unwrap())
        .with_file_name(format!("{name}.png"));
    std::fs::write(path, texture.save_to_png_bytes()).unwrap();
    different
}

fn geometry_valid(rect: (i32, i32, i32, i32), visible: (i32, i32, i32, i32), end: bool) -> bool {
    let (x, y, width, height) = rect;
    let (vx, vy, vw, vh) = visible;
    width >= 0
        && (end || width > 0)
        && height > 0
        && vw > 0
        && vh > 0
        && i64::from(x) >= i64::from(vx)
        && i64::from(x) + i64::from(width) <= i64::from(vx) + i64::from(vw)
        && i64::from(y) >= i64::from(vy)
        && i64::from(y) + i64::from(height) <= i64::from(vy) + i64::from(vh)
}

fn progresses(ys: &[i32]) -> bool {
    ys.windows(2).all(|w| w[0] < w[1])
}

#[test]
#[ignore = "synthetic bounded load phase probe"]
fn composer_load_probe() {
    use crate::native_composer::formatting::probe;
    libadwaita::init().unwrap();
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_enable_undo(true);
    let drafts = Rc::new(Cell::new(0u64));
    let changed = drafts.clone();
    let controller = ComposerView::builder()
        .launch(ComposerViewInit {
            state: ComposerState {
                active: true,
                editable: true,
                link_previews: false,
                ..Default::default()
            },
            buffer: buffer.clone(),
            enter_sends: Default::default(),
            recording_meter_area: gtk::DrawingArea::new(),
        })
        .connect_receiver(move |_, output| {
            if let ComposerViewOutput::DraftChanged(_) = output {
                changed.set(changed.get() + 1);
            }
        });
    assert!(controller.model().text_view().is_editable());
    assert!(buffer.tag_table().lookup(LINK_TAG).is_some());
    probe::start();
    std::fs::write(
        std::env::var("ZMD_LOAD_MAPS").unwrap(),
        std::fs::read("/proc/self/maps").unwrap(),
    )
    .unwrap();
    probe::event("fixture_read_enter", 0);
    let text = std::fs::read_to_string(std::env::var("ZMD_FIXTURE").unwrap()).unwrap();
    probe::event("fixture_read_exit", text.len());
    assert!(text.is_ascii() && !text.contains('\n'));
    probe::event("set_text_enter", text.len());
    buffer.set_text(&text);
    probe::event("set_text_exit", text.len());
    probe::event("postassert_enter", 0);
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        text
    );
    assert_eq!(buffer.char_count() as usize, text.chars().count());
    let context = gtk::glib::MainContext::default();
    while context.pending() {
        context.iteration(false);
    }
    assert!(drafts.get() > 0);
    probe::event("postassert_exit", drafts.get() as usize);
}

fn visible_at(
    view: &gtk::TextView,
    iter: &gtk::TextIter,
    offset: i32,
    end: bool,
) -> (i32, i32, i32, i32) {
    assert_eq!(iter.offset(), offset);
    let rect = view.iter_location(iter);
    let visible = view.visible_rect();
    let r = (rect.x(), rect.y(), rect.width(), rect.height());
    let v = (visible.x(), visible.y(), visible.width(), visible.height());
    assert!(
        geometry_valid(r, v, end),
        "target outside viewport: offset={offset} rect={r:?} visible={v:?}"
    );
    if !end {
        let inverse = view
            .iter_at_location(rect.x() + rect.width() / 2, rect.y() + rect.height() / 2)
            .expect("internal point has no iterator");
        assert_eq!(inverse.offset(), offset, "ASCII point roundtrip");
    }
    assert_eq!(iter.offset(), offset);
    r
}

fn wait_for_paint(window: &gtk::Window, paints: &Cell<u64>) {
    let before = paints.get();
    window.queue_draw();
    let until = Instant::now() + Duration::from_secs(2);
    let context = gtk::glib::MainContext::default();
    while paints.get() == before && Instant::now() < until {
        while context.pending() {
            context.iteration(false);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(paints.get() > before, "after-paint did not complete");
}

#[test]
#[ignore = "synthetic isolated display resource measurement"]
fn composer_resource() {
    assert!(!geometry_valid(
        (1197208, 129947, 7, 18),
        (0, 129931, 288, 34),
        false
    ));
    assert!(geometry_valid((0, 0, 7, 18), (0, -8, 288, 34), false));
    assert!(
        !progresses(&[0, 129947, 129947]),
        "saved plateau must fail monotonicity"
    );
    assert!(progresses(&[0, 6755, 13510]));
    libadwaita::init().unwrap();
    let buffer = gtk::TextBuffer::new(None);
    buffer.set_enable_undo(true);
    let drafts = Rc::new(Cell::new(0u64));
    let changed = drafts.clone();
    let controller = ComposerView::builder()
        .launch(ComposerViewInit {
            state: ComposerState {
                active: true,
                editable: true,
                link_previews: false,
                ..Default::default()
            },
            buffer: buffer.clone(),
            enter_sends: Default::default(),
            recording_meter_area: gtk::DrawingArea::new(),
        })
        .connect_receiver(move |_, output| {
            if let ComposerViewOutput::DraftChanged(_) = output {
                changed.set(changed.get() + 1);
            }
        });
    let view = controller.model().text_view();
    let window = gtk::Window::builder()
        .default_width(500)
        .default_height(300)
        .child(controller.widget())
        .build();
    assert!(view.is_editable());
    assert!(buffer.tag_table().lookup(LINK_TAG).is_some());
    event(
        "construct",
        serde_json::json!({"assertions":2,"preview_enabled":false}),
    );
    let text = std::fs::read_to_string(std::env::var("ZMD_FIXTURE").unwrap()).unwrap();
    assert!(text.is_ascii() && !text.contains('\n'));
    buffer.set_text(&text);
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        text
    );
    assert_eq!(buffer.char_count() as usize, text.chars().count());
    event(
        "load",
        serde_json::json!({"assertions":2,"bytes":text.len(),"characters":buffer.char_count()}),
    );
    let frames = Rc::new(Cell::new(0u64));
    let seen = frames.clone();
    window.add_tick_callback(move |_, _| {
        seen.set(seen.get() + 1);
        gtk::glib::ControlFlow::Continue
    });
    window.present();
    settle();
    assert!(frames.get() > 0 && view.is_mapped());
    let paints = Rc::new(Cell::new(0u64));
    let painted = paints.clone();
    window
        .frame_clock()
        .expect("mapped window frame clock")
        .connect_after_paint(move |_| painted.set(painted.get() + 1));
    wait_for_paint(&window, &paints);
    let varied = snapshot(&window, "first-frame");
    event(
        "first_frame",
        serde_json::json!({"assertions":3,"ticks":frames.get(),"after_paint":paints.get(),"saved_bad_rejected":true,"different_pixels":varied,"artifact":"first-frame.png"}),
    );
    let scroll = view
        .ancestor(gtk::ScrolledWindow::static_type())
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    let adj = scroll.vadjustment();
    let (lower, upper, page) = (adj.lower(), adj.upper(), adj.page_size());
    assert!(
        view.width() > 0
            && view.height() > 0
            && upper.is_finite()
            && page > 0.0
            && upper - page > lower,
        "INCOMPLETE invalid or missing scroll extent upper={upper} page={page} lower={lower}"
    );
    assert_eq!(page, f64::from(view.height()));
    event(
        "geometry",
        serde_json::json!({"assertions":2,"width":view.width(),"height":view.height(),"lower":lower,"upper":upper,"page":page}),
    );
    let mut points = Vec::new();
    for (name, offset) in [
        ("start", 0),
        ("middle", buffer.char_count() / 2),
        ("end", buffer.char_count()),
    ] {
        let mut iter = buffer.iter_at_offset(offset);
        assert_eq!(iter.offset(), offset);
        view.scroll_to_iter(&mut iter, 0.0, false, 0.0, 0.0);
        assert_eq!(iter.offset(), offset);
        settle();
        wait_for_paint(&window, &paints);
        let rect = visible_at(&view, &iter, offset, offset == buffer.char_count());
        let variation = snapshot(&window, name);
        points.push(serde_json::json!({"point":name,"offset":offset,"rect":rect,"adjustment":adj.value(),"different_pixels":variation,"artifact":format!("{name}.png")}));
    }
    let ys: Vec<i32> = points
        .iter()
        .map(|p| p["rect"][1].as_i64().unwrap() as i32)
        .collect();
    assert!(progresses(&ys), "wrapped sample y did not progress: {ys:?}");
    adj.set_value(adj.lower());
    wait_for_paint(&window, &paints);
    let mut steps = 0u64;
    let mut last_visible_y = view.visible_rect().y();
    loop {
        let before = adj.value();
        let limit = adj.upper() - adj.page_size();
        if before >= limit - 0.5 {
            break;
        }
        let target = (before + adj.page_size()).min(limit);
        adj.set_value(target);
        wait_for_paint(&window, &paints);
        let actual = adj.value();
        let y = view.visible_rect().y();
        assert!(
            actual > before && (actual - target).abs() < 1.0,
            "scroll did not reach target before={before} target={target} actual={actual}"
        );
        assert!(
            y > last_visible_y,
            "visible viewport did not progress {last_visible_y} -> {y}"
        );
        last_visible_y = y;
        steps += 1;
    }
    let final_rect = visible_at(&view, &buffer.end_iter(), buffer.char_count(), true);
    assert!(
        (adj.value() - (adj.upper() - adj.page_size())).abs() < 1.0,
        "end not reached"
    );
    snapshot(&window, "end-extent");
    points.push(serde_json::json!({"point":"end-extent","rect":final_rect,"adjustment":adj.value(),"artifact":"end-extent.png"}));
    event(
        "scroll",
        serde_json::json!({"assertions":10,"points":points,"end":adj.value(),"viewport_steps":steps,"after_paint":paints.get(),"sample_y":ys}),
    );
    let offset = buffer.char_count() / 2;
    buffer.place_cursor(&buffer.iter_at_offset(offset));
    let changes_before = drafts.get();
    view.emit_by_name::<()>("insert-at-cursor", &[&"Q"]);
    assert_eq!(buffer.char_count() as usize, text.chars().count() + 1);
    assert_eq!(
        buffer.iter_at_mark(&buffer.get_insert()).offset(),
        offset + 1
    );
    assert!(buffer.can_undo());
    buffer.undo();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        text
    );
    assert!(buffer.can_redo());
    buffer.redo();
    assert_eq!(buffer.char_count() as usize, text.chars().count() + 1);
    buffer.undo();
    assert_eq!(
        buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
        text
    );
    settle();
    assert!(
        drafts.get() > changes_before,
        "production draft callbacks did not dispatch"
    );
    event(
        "edit",
        serde_json::json!({"assertions":8,"cursor_offset":offset,"draft_callbacks":drafts.get()-changes_before}),
    );
    buffer.select_range(
        &buffer.iter_at_offset(offset),
        &buffer.iter_at_offset(offset + 1),
    );
    let (start, end) = buffer.selection_bounds().expect("selection missing");
    assert_eq!((start.offset(), end.offset()), (offset, offset + 1));
    event(
        "selection",
        serde_json::json!({"assertions":2,"start":start.offset(),"end":end.offset()}),
    );
    let previous = frames.get();
    settle();
    assert!(frames.get() > previous);
    wait_for_paint(&window, &paints);
    snapshot(&window, "settled");
    let maps_path = std::path::PathBuf::from(std::env::var("ZMD_EVENTS").unwrap())
        .with_file_name("loaded-maps.txt");
    std::fs::write(maps_path, std::fs::read("/proc/self/maps").unwrap()).unwrap();
    let font = view
        .pango_context()
        .font_description()
        .map(|f| f.to_string())
        .unwrap_or_default();
    event(
        "settled",
        serde_json::json!({"assertions":3,"ticks":frames.get(),"after_paint":paints.get(),"font_description":font,"gtk_version":format!("{}.{}.{}", gtk::major_version(), gtk::minor_version(), gtk::micro_version()),"scale":window.scale_factor()}),
    );
    window.close();
    settle();
    assert!(!window.is_visible());
    assert!(!window.is_mapped());
    event("destroy", serde_json::json!({"assertions":2}));
}
