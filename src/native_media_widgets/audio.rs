use super::*;

/// Draws voice-message bars (0-100) across the area; with `progress`, the played
/// part is brighter and a dot marks the position. Shared by playback and recording.
fn draw_waveform(
    area: &gtk::DrawingArea,
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    bars: &[u8],
    progress: Option<f32>,
) {
    let n = bars.len();
    if n == 0 || width <= 0 {
        return;
    }
    let color = area.color();
    // Leave room for the progress dot at both ends.
    let dot = 5.0;
    let bar_width = (width as f64 - 2.0 * dot) / n as f64;
    let played = progress.map_or(1.0, |p| p.clamp(0.0, 1.0));
    for (index, value) in bars.iter().enumerate() {
        let h = (f64::from(*value.min(&100)) / 100.0 * (height - 4) as f64).max(3.0);
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            if (index as f32) < played * n as f32 {
                0.9
            } else {
                0.35
            },
        );
        cr.rectangle(
            dot + index as f64 * bar_width,
            (height as f64 - h) / 2.0,
            (bar_width - 2.0).max(1.0),
            h,
        );
        let _ = cr.fill();
    }
    if progress.is_some() {
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            1.0,
        );
        cr.arc(
            dot + f64::from(played) * (width as f64 - 2.0 * dot),
            height as f64 / 2.0,
            dot,
            0.0,
            std::f64::consts::TAU,
        );
        let _ = cr.fill();
    }
}

/// Live microphone meter: the most recent loudness readings, drawn like a
/// voice-message waveform so both look the same.
#[derive(Clone)]
pub struct RecordingMeter {
    pub area: gtk::DrawingArea,
    bars: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
}

impl RecordingMeter {
    pub fn new() -> Self {
        let bars = std::rc::Rc::new(std::cell::RefCell::new(vec![0; crate::voice::BARS]));
        let area = gtk::DrawingArea::builder()
            .content_width(160)
            .content_height(32)
            .hexpand(true)
            .build();
        area.update_property(&[gtk::accessible::Property::Label("Microphone level")]);
        let paint = bars.clone();
        area.set_draw_func(move |area, cr, width, height| {
            draw_waveform(area, cr, width, height, &paint.borrow(), None);
        });
        Self { area, bars }
    }

    /// Shows the latest readings, entering from the right of the message-sized row.
    pub fn set_levels(&self, levels: &[f32]) {
        let recent = &levels[levels.len().saturating_sub(crate::voice::BARS)..];
        let peak = recent
            .iter()
            .copied()
            .filter(|level| level.is_finite())
            .fold(0.02_f32, f32::max);
        let mut bars = vec![0; crate::voice::BARS - recent.len()];
        bars.extend(recent.iter().map(|level| {
            if level.is_finite() {
                ((level / peak).clamp(0.0, 1.0) * 100.0).round() as u8
            } else {
                0
            }
        }));
        *self.bars.borrow_mut() = bars;
        self.area.queue_draw();
    }
}

impl Default for RecordingMeter {
    fn default() -> Self {
        Self::new()
    }
}

/// Row-local GTK controls. Updating these avoids stealing focus during playback.
#[derive(Clone)]
pub struct AudioControls {
    pub widget: gtk::Box,
    button: gtk::Button,
    seek: gtk::Scale,
    time: gtk::Label,
    speed: gtk::Button,
    error: gtk::Label,
    waveform: gtk::DrawingArea,
    bars: std::rc::Rc<std::cell::RefCell<(Vec<u8>, f32)>>,
}

impl AudioControls {
    pub fn new(
        voice: &crate::native_voice::VoiceMessage,
        voice_note: bool,
        on_action: impl Fn(crate::native_voice::VoiceIntent) + Clone + 'static,
    ) -> Self {
        let widget = gtk::Box::builder().spacing(8).build();
        widget.add_css_class("zaptide-audio-controls");
        let button = gtk::Button::builder()
            .css_classes(["circular", "flat"])
            .build();
        let action = on_action.clone();
        button.connect_clicked(move |_| action(crate::native_voice::VoiceIntent::Activate));
        widget.append(&button);

        let overlay = gtk::Overlay::new();
        overlay.set_hexpand(true);
        let seek = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
        seek.set_draw_value(false);
        // The waveform shows progress; the scale only takes clicks and drags.
        seek.add_css_class("zaptide-audio-seek");
        seek.update_property(&[gtk::accessible::Property::Label("Seek audio")]);
        let action = on_action.clone();
        seek.connect_change_value(move |_, _, value| {
            action(crate::native_voice::VoiceIntent::Seek(
                value.clamp(0.0, 1.0) as f32,
            ));
            gtk::glib::Propagation::Stop
        });

        let bars = std::rc::Rc::new(std::cell::RefCell::new((Vec::<u8>::new(), 0.0_f32)));
        let waveform = gtk::DrawingArea::builder()
            .content_width(160)
            .content_height(32)
            .build();
        waveform.set_hexpand(true);
        let paint = bars.clone();
        waveform.set_draw_func(move |area, cr, width, height| {
            let values = paint.borrow();
            draw_waveform(area, cr, width, height, &values.0, Some(values.1));
        });
        overlay.set_child(Some(&waveform));
        overlay.add_overlay(&seek);
        widget.append(&overlay);
        let time = gtk::Label::builder()
            .css_classes(["numeric", "dim-label"])
            .build();
        widget.append(&time);
        let speed = gtk::Button::builder()
            .css_classes(["flat", "zaptide-audio-speed"])
            .width_request(58)
            .build();
        speed.set_visible(voice_note);
        let action = on_action;
        speed.connect_clicked(move |_| action(crate::native_voice::VoiceIntent::CycleSpeed));
        widget.append(&speed);
        let error = gtk::Label::builder()
            .css_classes(["error", "caption"])
            .wrap(true)
            .visible(false)
            .build();
        // Keep error alongside the controls instead of relegating it to a toast.
        let container = gtk::Box::new(gtk::Orientation::Vertical, 2);
        container.append(&widget);
        container.append(&error);
        let controls = Self {
            widget: container,
            button,
            seek,
            time,
            speed,
            error,
            waveform,
            bars,
        };
        controls.update(voice);
        controls
    }

    pub fn update(&self, voice: &crate::native_voice::VoiceMessage) {
        use crate::native_voice::VoiceControl;
        let (icon, label) = match voice.control {
            VoiceControl::Download => ("folder-download-symbolic", "Download audio"),
            VoiceControl::Loading => ("content-loading-symbolic", "Loading audio"),
            VoiceControl::Play => ("media-playback-start-symbolic", "Play audio"),
            VoiceControl::Pause => ("media-playback-pause-symbolic", "Pause audio"),
        };
        self.button.set_icon_name(icon);
        self.button.set_tooltip_text(Some(label));
        self.button
            .update_property(&[gtk::accessible::Property::Label(label)]);
        self.button
            .set_sensitive(voice.control != VoiceControl::Loading);
        self.seek
            .set_sensitive(voice.speed.is_some() && voice.control != VoiceControl::Loading);
        self.seek.set_value(f64::from(voice.progress));
        self.time.set_label(&voice.time);
        let speed_label = voice.speed.as_deref().unwrap_or("1x");
        self.speed.set_label(speed_label);
        if speed_label == "1.5x" {
            self.speed.add_css_class("compact");
        } else {
            self.speed.remove_css_class("compact");
        }
        *self.bars.borrow_mut() = (voice.waveform.clone(), voice.progress);
        self.waveform.queue_draw();
        if let Some(error) = &voice.error {
            self.button.set_tooltip_text(Some(error));
        }
        self.error
            .set_label(voice.error.as_deref().unwrap_or_default());
        self.error.set_visible(voice.error.is_some());
    }
}
