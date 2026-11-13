//! Offline demo data and window captures for the website and release notes.
//!
//! Everything here is invented: names, numbers, messages and pictures. The
//! backend is replaced by a detached one, so nothing is read from or sent to
//! WhatsApp and no archive is opened.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

use relm4::ComponentSender;
use relm4::adw::prelude::*;
use relm4::gtk::{self, glib, graphene};

use super::{Input, NativeApplication};
use crate::backend::{Event, LinkStatus};
use crate::model::{
    Chat, ChatKind, Content, Delivery, LastMessage, Media, Message, PollState, Quoted, Reaction,
};
use crate::notifier::EventNotifier;

const ME: &str = "15550199@s.whatsapp.net";
const MAYA: &str = "15550101@s.whatsapp.net";
const NOAH: &str = "15550102@s.whatsapp.net";
const PRIYA: &str = "15550103@s.whatsapp.net";
const LEO: &str = "15550104@s.whatsapp.net";
const STUDIO: &str = "120363000000000001@g.us";
const HIKING: &str = "120363000000000002@g.us";
const FAMILY: &str = "120363000000000003@g.us";
const BOOKS: &str = "120363000000000004@g.us";
const TRIP: &str = "120363000000000005@g.us";

/// The folder that receives the captures, when demo mode is on.
pub(super) fn output_dir() -> Option<PathBuf> {
    std::env::var_os("ZAPTIDE_DEMO_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn today_at(hour: i32, minute: i32) -> i64 {
    glib::DateTime::now_local()
        .and_then(|now| {
            glib::DateTime::from_local(
                now.year(),
                now.month(),
                now.day_of_month(),
                hour,
                minute,
                0.0,
            )
        })
        .map(|moment| moment.to_unix())
        .unwrap_or_default()
}

fn minutes_ago(minutes: i64) -> i64 {
    today_at(14, 40) - minutes * 60
}

struct Palette {
    sky_top: [u8; 3],
    sky_bottom: [u8; 3],
    sun: [u8; 3],
    far: [u8; 3],
    near: [u8; 3],
}

fn blend(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let mix = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)) as u8;
    [mix(a[0], b[0]), mix(a[1], b[1]), mix(a[2], b[2])]
}

fn landscape(path: &Path, width: u32, height: u32, palette: &Palette, phase: f32) {
    let mut picture = image::RgbaImage::new(width, height);
    let sun = (
        width as f32 * 0.68,
        height as f32 * 0.36,
        height as f32 * 0.11,
    );
    for y in 0..height {
        for x in 0..width {
            let fx = x as f32 / width as f32;
            let fy = y as f32 / height as f32;
            let mut color = blend(palette.sky_top, palette.sky_bottom, fy * 1.3);
            let distance = ((x as f32 - sun.0).powi(2) + (y as f32 - sun.1).powi(2)).sqrt();
            if distance < sun.2 {
                color = palette.sun;
            } else if distance < sun.2 * 1.9 {
                color = blend(
                    color,
                    palette.sun,
                    0.25 * (1.0 - (distance - sun.2) / sun.2),
                );
            }
            let far = 0.58 + 0.07 * (fx * 7.0 + phase).sin() + 0.03 * (fx * 19.0).sin();
            let near = 0.72 + 0.06 * (fx * 5.0 + phase * 2.0).sin() + 0.02 * (fx * 23.0).sin();
            if fy > far {
                color = palette.far;
            }
            if fy > near {
                color = palette.near;
            }
            picture.put_pixel(x, y, image::Rgba([color[0], color[1], color[2], 255]));
        }
    }
    picture.save(path).expect("demo picture");
}

fn avatar(path: &Path, top: [u8; 3], bottom: [u8; 3], phase: f32) {
    landscape(
        path,
        128,
        128,
        &Palette {
            sky_top: top,
            sky_bottom: bottom,
            sun: [255, 236, 190],
            far: blend(bottom, [30, 50, 70], 0.55),
            near: blend(bottom, [10, 25, 40], 0.8),
        },
        phase,
    );
}

fn voice_note(path: &Path) -> Vec<u8> {
    let rate = 48_000usize;
    let seconds = 14usize;
    let mut seed = 7u32;
    let samples: Vec<f32> = (0..rate * seconds)
        .map(|index| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 16) as f32 / 32_768.0 - 1.0;
            let time = index as f32 / rate as f32;
            let syllable = (time * 5.3).sin().abs().powf(0.6) * (0.6 + 0.4 * (time * 1.7).sin());
            let pause = if (time * 0.55).fract() > 0.88 {
                0.05
            } else {
                1.0
            };
            let tone = (time * 2.0 * std::f32::consts::PI * 180.0).sin() * 0.5
                + (time * 2.0 * std::f32::consts::PI * 360.0).sin() * 0.25;
            (tone + noise * 0.08) * syllable * pause * 0.5
        })
        .collect();
    std::fs::write(
        path,
        crate::voice::encode(&samples).expect("demo voice note"),
    )
    .expect("demo voice file");
    crate::voice::waveform(&samples)
}

fn media(
    mime: &str,
    size: u64,
    path: Option<PathBuf>,
    width: Option<u32>,
    height: Option<u32>,
) -> Media {
    Media {
        mime: mime.into(),
        size,
        width,
        height,
        path,
        state: Default::default(),
    }
}

#[allow(clippy::too_many_arguments)]
fn message(
    id: &str,
    chat: &str,
    sender: &str,
    name: Option<&str>,
    from_me: bool,
    timestamp: i64,
    content: Content,
    status: Delivery,
) -> Message {
    Message {
        id: id.into(),
        chat: chat.into(),
        sender: sender.into(),
        sender_name: name.map(str::to_owned),
        from_me,
        timestamp,
        content,
        status,
        delivered_at: None,
        read_at: None,
        quoted: None,
        reactions: Vec::new(),
        edited: false,
        mentions: Vec::new(),
        forwarded: false,
        thumbnail: None,
    }
}

fn text(body: &str) -> Content {
    Content::Text {
        text: body.into(),
        preview: None,
    }
}

fn chat(
    id: &str,
    name: &str,
    activity: i64,
    summary: &str,
    from_me: bool,
    status: Delivery,
    sender_name: Option<&str>,
) -> Chat {
    let mut chat = Chat::new(id.into(), name.into());
    chat.last_activity = activity;
    chat.last = Some(LastMessage {
        from_me,
        sender: if from_me { ME.into() } else { id.into() },
        sender_name: sender_name.map(str::to_owned),
        summary: summary.into(),
        status,
    });
    chat
}

fn chats() -> Vec<Chat> {
    let mut studio = chat(
        STUDIO,
        "Studio Team",
        minutes_ago(2),
        "Mockups are ready for review",
        false,
        Delivery::None,
        Some("Priya"),
    );
    studio.pinned = true;
    studio.pinned_at = 3;
    studio.kind = ChatKind::Group;
    let mut maya = chat(
        MAYA,
        "Maya Santos",
        minutes_ago(4),
        "Perfect, café at 7! ☕",
        false,
        Delivery::None,
        None,
    );
    maya.pinned = true;
    maya.pinned_at = 2;
    let mut hiking = chat(
        HIKING,
        "Hiking Club",
        minutes_ago(18),
        "Leo: Trail is open again",
        false,
        Delivery::None,
        Some("Leo"),
    );
    hiking.unread = 3;
    hiking.kind = ChatKind::Group;
    let mut noah = chat(
        NOAH,
        "Noah Fischer",
        minutes_ago(47),
        "Sent you the invoice",
        true,
        Delivery::Read,
        None,
    );
    noah.pinned = false;
    let mut family = chat(
        FAMILY,
        "Family",
        minutes_ago(95),
        "Dinner is at 8",
        false,
        Delivery::None,
        Some("Mom"),
    );
    family.unread = 2;
    family.muted_until = Some(0);
    family.kind = ChatKind::Group;
    let mut books = chat(
        BOOKS,
        "Book Circle",
        minutes_ago(180),
        "Chapter 7 is wild",
        false,
        Delivery::None,
        Some("Ana"),
    );
    books.kind = ChatKind::Group;
    let priya = chat(
        PRIYA,
        "Priya Nair",
        minutes_ago(300),
        "Thanks again!",
        true,
        Delivery::Delivered,
        None,
    );
    let mut trip = chat(
        TRIP,
        "Weekend Trip",
        minutes_ago(24 * 60 + 40),
        "Photo",
        false,
        Delivery::None,
        Some("Sam"),
    );
    trip.kind = ChatKind::Group;
    let leo = chat(
        LEO,
        "Leo Martins",
        minutes_ago(2 * 24 * 60 + 30),
        "Call me when you can",
        false,
        Delivery::None,
        None,
    );
    vec![studio, maya, hiking, noah, family, books, priya, trip, leo]
}

fn maya_conversation() -> Vec<Message> {
    let at = |m: i64| minutes_ago(m);
    let mut quoted_reply = message(
        "m6",
        MAYA,
        ME,
        None,
        true,
        at(9),
        text("Yes, the little place by the river. I booked a table for two."),
        Delivery::Read,
    );
    quoted_reply.quoted = Some(Quoted {
        id: "m5".into(),
        sender: MAYA.into(),
        sender_name: Some("Maya Santos".into()),
        summary: "Do you still want to try the new restaurant?".into(),
        mentions: Vec::new(),
    });
    let mut liked = message(
        "m8",
        MAYA,
        MAYA,
        Some("Maya Santos"),
        false,
        at(6),
        text("You are the best, thank you!"),
        Delivery::None,
    );
    liked.reactions = vec![Reaction {
        sender: ME.into(),
        from_me: true,
        emoji: "❤️".into(),
    }];
    vec![
        message(
            "m1",
            MAYA,
            MAYA,
            Some("Maya Santos"),
            false,
            at(41),
            text("Morning! Did you see the schedule for the weekend?"),
            Delivery::None,
        ),
        message(
            "m2",
            MAYA,
            ME,
            None,
            true,
            at(39),
            text("Just did. Saturday looks open for me."),
            Delivery::Read,
        ),
        message(
            "m3",
            MAYA,
            MAYA,
            Some("Maya Santos"),
            false,
            at(38),
            text("Great. I was thinking dinner after the gallery."),
            Delivery::None,
        ),
        message(
            "m4",
            MAYA,
            ME,
            None,
            true,
            at(37),
            text("Sounds perfect. I will bring the tickets."),
            Delivery::Read,
        ),
        message(
            "m5",
            MAYA,
            MAYA,
            Some("Maya Santos"),
            false,
            at(12),
            text("Do you still want to try the new restaurant?"),
            Delivery::None,
        ),
        quoted_reply,
        message(
            "m7",
            MAYA,
            MAYA,
            Some("Maya Santos"),
            false,
            at(7),
            text("Nice! Should I wear something warm?"),
            Delivery::None,
        ),
        liked,
        message(
            "m9",
            MAYA,
            MAYA,
            Some("Maya Santos"),
            false,
            at(4),
            text("Perfect, café at 7! ☕"),
            Delivery::None,
        ),
    ]
}

fn studio_conversation(assets: &Path) -> Vec<Message> {
    let at = |m: i64| minutes_ago(m);
    let picture = assets.join("mockup.png");
    landscape(
        &picture,
        720,
        380,
        &Palette {
            sky_top: [64, 112, 190],
            sky_bottom: [246, 190, 150],
            sun: [255, 244, 210],
            far: [86, 98, 140],
            near: [38, 52, 84],
        },
        0.8,
    );
    let voice = assets.join("note.ogg");
    let waveform = voice_note(&voice);
    let mut image_message = message(
        "s3",
        STUDIO,
        PRIYA,
        Some("Priya"),
        false,
        at(15),
        Content::Image {
            caption: Some("Cover option B, warmer light".into()),
            media: media("image/png", 412_000, Some(picture), Some(720), Some(380)),
        },
        Delivery::None,
    );
    image_message.reactions = vec![
        Reaction {
            sender: ME.into(),
            from_me: true,
            emoji: "👍".into(),
        },
        Reaction {
            sender: NOAH.into(),
            from_me: false,
            emoji: "🔥".into(),
        },
    ];
    vec![
        message(
            "s1",
            STUDIO,
            NOAH,
            Some("Noah"),
            false,
            at(60),
            text("Kickoff notes are in the shared folder."),
            Delivery::None,
        ),
        message(
            "s2",
            STUDIO,
            ME,
            None,
            true,
            at(55),
            text("Thanks, reading them now."),
            Delivery::Read,
        ),
        message(
            "s5",
            STUDIO,
            NOAH,
            Some("Noah"),
            false,
            at(30),
            Content::Document {
                media: media("application/pdf", 2_400_000, None, None, None),
                file_name: "Brand-guidelines.pdf".into(),
                caption: Some("Final copy of the guidelines".into()),
                pages: Some(18),
            },
            Delivery::None,
        ),
        image_message,
        message(
            "s4",
            STUDIO,
            ME,
            None,
            true,
            at(11),
            Content::Audio {
                media: media("audio/ogg; codecs=opus", 28_000, Some(voice), None, None),
                seconds: Some(14),
                voice_note: true,
                waveform,
            },
            Delivery::Played,
        ),
        message(
            "s6",
            STUDIO,
            LEO,
            Some("Leo"),
            false,
            at(5),
            Content::Poll {
                question: "Where should we have lunch on Friday?".into(),
                options: vec![
                    "Taqueria".into(),
                    "Noodle bar".into(),
                    "Picnic in the park".into(),
                ],
                state: PollState {
                    selectable: 1,
                    counts: vec![1, 3, 2],
                    selected: vec![1],
                    voters: 6,
                    can_vote: true,
                    history_complete: true,
                    ..Default::default()
                },
            },
            Delivery::None,
        ),
        message(
            "s7",
            STUDIO,
            PRIYA,
            Some("Priya"),
            false,
            at(2),
            text("Mockups are ready for review"),
            Delivery::None,
        ),
    ]
}

fn capture(widget: &impl IsA<gtk::Widget>, path: &Path) {
    let widget = widget.as_ref();
    let (width, height) = (widget.width(), widget.height());
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
    let Some(node) = snapshot.to_node() else {
        log::error!("demo capture has nothing to draw");
        return;
    };
    let Some(renderer) = widget.native().and_then(|native| native.renderer()) else {
        log::error!("demo capture has no renderer");
        return;
    };
    let area = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&area));
    if let Err(error) = texture.save_to_png(path) {
        log::error!("demo capture failed: {error}");
    } else {
        log::info!("demo capture saved to {}", path.display());
    }
}

/// Feeds the interface with invented data and saves the scenes the website uses.
pub(super) fn start(
    events: Sender<Event>,
    notifier: EventNotifier,
    sender: ComponentSender<NativeApplication>,
    window: gtk::Window,
    directory: PathBuf,
) {
    let assets = directory.join("assets");
    std::fs::create_dir_all(&assets).expect("demo folder");
    let name = std::env::var("ZAPTIDE_DEMO_NAME").unwrap_or_else(|_| "demo".into());
    let (width, height) = (1100, 760);
    window.set_default_size(width, height);

    let send = {
        let events = events.clone();
        let notifier = notifier.clone();
        move |event: Event| {
            let _ = events.send(event);
            crate::backend::Wake::wake(&notifier);
        }
    };
    send(Event::Link(LinkStatus::Connected));
    send(Event::Profile {
        phone: Some("+1 555 0199".into()),
        name: Some("Alex Rivera".into()),
    });
    send(Event::Chats(chats()));
    send(Event::Contacts(
        [
            (MAYA, "Maya Santos"),
            (NOAH, "Noah"),
            (PRIYA, "Priya"),
            (LEO, "Leo"),
        ]
        .into_iter()
        .map(|(id, name)| crate::model::Contact {
            id: id.into(),
            full_name: Some(name.into()),
            push_name: Some(name.into()),
        })
        .collect(),
    ));
    send(Event::Presence {
        id: MAYA.into(),
        online: true,
        last_seen: None,
    });
    let tints = [
        (MAYA, [236, 160, 120], [90, 120, 190], 0.2),
        (NOAH, [120, 180, 210], [240, 230, 200], 1.1),
        (PRIYA, [170, 120, 200], [250, 190, 170], 2.0),
        (TRIP, [250, 200, 120], [70, 130, 160], 2.8),
        (BOOKS, [120, 170, 140], [235, 220, 170], 3.4),
    ];
    for (id, top, bottom, phase) in tints {
        let path = assets.join(format!("avatar-{}.png", &id[..8]));
        avatar(&path, top, bottom, phase);
        send(Event::Avatar {
            id: id.into(),
            full: false,
            path: Some(path),
        });
    }

    let open = |chat: &'static str, messages: Vec<Message>, at: u64| {
        let (sender, send) = (sender.clone(), send.clone());
        glib::timeout_add_local_once(Duration::from_millis(at), move || {
            sender.input(Input::OpenChatId(chat.into()));
            let send = send.clone();
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                send(Event::Messages {
                    chat: chat.into(),
                    messages,
                    older: false,
                    complete: true,
                    request: None,
                });
            });
        });
    };
    open(MAYA, maya_conversation(), 1500);
    let shot = |label: &'static str, at: u64| {
        let (window, directory, name) = (window.clone(), directory.clone(), name.clone());
        glib::timeout_add_local_once(Duration::from_millis(at), move || {
            capture(&window, &directory.join(format!("{name}-{label}.png")));
        });
    };
    shot("chat", 4000);
    open(STUDIO, studio_conversation(&assets), 5000);
    let typing = send.clone();
    glib::timeout_add_local_once(Duration::from_millis(7000), move || {
        typing(Event::Typing {
            chat: STUDIO.into(),
            sender: PRIYA.into(),
            composing: true,
        });
    });
    shot("messages", 8000);
    glib::timeout_add_local_once(Duration::from_millis(9000), || {
        relm4::main_application().quit();
    });
}
