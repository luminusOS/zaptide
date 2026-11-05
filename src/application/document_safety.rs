//! Received documents open directly only when every type GIO can see in
//! them is a known passive format; anything else asks first and then goes
//! through an app chooser where the desktop offers one.

use std::io::Read;
use std::path::Path;

use adw::prelude::*;
use relm4::{adw, gtk};

use crate::i18n::gettext;

const SNIFF_BYTES: u64 = 4096;

/// Passive formats that are not covered by a whole family below.
const SAFE_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "image/avif",
    "image/heif",
    "image/heic",
    "image/bmp",
    "image/tiff",
    "image/jxl",
    // Camera RAW, as the shared MIME database names DNG, CR2, CR3, NEF, ARW,
    // ORF, RW2 and RAF files.
    "image/x-adobe-dng",
    "image/x-canon-cr2",
    "image/x-canon-cr3",
    "image/x-nikon-nef",
    "image/x-sony-arw",
    "image/x-olympus-orf",
    "image/x-panasonic-rw2",
    "image/x-fuji-raf",
    "application/pdf",
    "application/epub+zip",
    "application/rtf",
    "text/rtf",
    "application/msword",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "text/plain",
    "text/markdown",
    "text/csv",
    "application/zip",
    "application/x-tar",
    "application/gzip",
    "application/x-compressed-tar",
    "application/x-7z-compressed",
    "application/vnd.rar",
    "application/x-rar",
    "application/x-rar-compressed",
    // What the content sniffer reports for the formats above: Ogg audio,
    // Matroska and ASF (WMV and WMA) media, legacy Office files and empty
    // files.
    "application/ogg",
    "application/x-matroska",
    "application/vnd.ms-asf",
    "application/x-ole-storage",
    "application/x-zerosize",
];

const SAFE_FAMILIES: &[&str] = &[
    "audio/",
    "video/",
    "application/vnd.oasis.opendocument.",
    "application/vnd.openxmlformats-officedocument.",
];

/// Playlists sit among audio and video types but point the player at
/// arbitrary URLs. Text-based ones are caught by their text/plain parent.
const PLAYLISTS: &[&str] = &[
    "audio/x-mpegurl",
    "audio/mpegurl",
    "audio/x-scpls",
    "audio/x-ms-asx",
    "video/vnd.mpegurl",
    "video/x-ms-wvx",
];

/// Declared types that on their own are reason to ask; others only describe
/// the file, which the name and content checks already judge.
const RISKY_DECLARED: &[&str] = &[
    "text/html",
    "application/xhtml+xml",
    "image/svg+xml",
    "application/x-desktop",
];

fn normalized(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

fn is_safe(content_type: &str) -> bool {
    let content_type = normalized(content_type);
    if SAFE_TYPES.contains(&content_type.as_str()) {
        return true;
    }
    let in_family = SAFE_FAMILIES
        .iter()
        .any(|family| content_type.len() > family.len() && content_type.starts_with(family));
    if !in_family {
        return false;
    }
    let media = content_type.starts_with("audio/") || content_type.starts_with("video/");
    !(media
        && (PLAYLISTS.contains(&content_type.as_str())
            || gtk::gio::content_type_is_a(&content_type, "text/plain")))
}

/// Whether the sender's declared type alone is reason to ask: active markup,
/// launchers, Microsoft executable formats or a binary GIO calls executable.
/// Unknown or merely unusual names (`application/x-zip-compressed`,
/// `application/octet-stream`) are not.
fn is_risky_declared(declared_mime: &str) -> bool {
    let declared = normalized(declared_mime);
    if declared.is_empty() {
        return false;
    }
    let content_type = gtk::gio::content_type_from_mime_type(&declared)
        .map(|content_type| content_type.to_string())
        .unwrap_or(declared);
    // GIO calls every text type executable; text opens in an editor.
    RISKY_DECLARED.contains(&content_type.as_str())
        || content_type.starts_with("application/x-ms")
        || (gtk::gio::content_type_can_be_executable(&content_type)
            && !gtk::gio::content_type_is_a(&content_type, "text/plain"))
}

/// Whether opening a file called `name`, declared by its sender as
/// `declared_mime` and starting with `head`, should ask first. It opens
/// directly only when the type its name implies, the type its content
/// alone sniffs as and the type GIO launches it with (name and content
/// together) are all passive formats, so a name that disagrees with its
/// content asks as well, and when the declared type is not itself risky.
pub(super) fn needs_confirmation(name: &str, declared_mime: &str, head: &[u8]) -> bool {
    let (named, _) = gtk::gio::content_type_guess(Some(name), None);
    let (sniffed, _) = gtk::gio::content_type_guess(None::<&str>, head);
    let (launched, _) = gtk::gio::content_type_guess(Some(name), head);
    !(is_safe(&named) && is_safe(&sniffed) && is_safe(&launched))
        || is_risky_declared(declared_mime)
}

/// Whether the file at `path` needs a confirmation before it is opened. A
/// file that cannot be read asks too.
pub(super) fn path_needs_confirmation(path: &Path, declared_mime: &str) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let mut head = Vec::new();
    match std::fs::File::open(path).and_then(|file| file.take(SNIFF_BYTES).read_to_end(&mut head)) {
        Ok(_) => needs_confirmation(&name, declared_mime, &head),
        Err(_) => true,
    }
}

/// The sender's file name without controls, invisible format characters
/// (bidirectional overrides, zero-width joiners) or line and paragraph
/// separators, so the dialog shows the name as it is stored.
fn displayable(name: &str) -> String {
    use gtk::glib::{Unichar, UnicodeType};
    name.chars()
        .filter(|ch| {
            !ch.is_control()
                && !matches!(
                    ch.unicode_type(),
                    UnicodeType::Format
                        | UnicodeType::LineSeparator
                        | UnicodeType::ParagraphSeparator
                )
        })
        .collect()
}

/// Asks before opening a risky document, naming it as the sender did, then
/// opens it through `gtk::FileLauncher` with an app chooser where the portal
/// provides one.
pub(super) fn confirm_and_open(window: &adw::ApplicationWindow, path: &Path, sender_name: &str) {
    let alert = adw::AlertDialog::new(
        Some(&gettext("Open This File?")),
        Some(
            &gettext(
                "“{name}” may contain programs or active content. Open it only if you trust the sender.",
            )
            .replace("{name}", &displayable(sender_name)),
        ),
    );
    alert.add_response("cancel", &gettext("Cancel"));
    alert.add_response("open", &gettext("Open"));
    alert.set_response_appearance("open", adw::ResponseAppearance::Destructive);
    alert.set_close_response("cancel");
    alert.set_default_response(Some("cancel"));
    let file = gtk::gio::File::for_path(path);
    let parent = window.clone();
    alert.connect_response(None, move |_, response| {
        if response != "open" {
            return;
        }
        let launcher = gtk::FileLauncher::new(Some(&file));
        launcher.set_always_ask(true);
        let toast_window = parent.clone();
        launcher.launch(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
            if let Err(error) = result
                && !error.matches(gtk::DialogError::Dismissed)
                && !error.matches(gtk::DialogError::Cancelled)
            {
                super::show_toast(&toast_window, &gettext("Could not open attachment"));
            }
        });
    });
    alert.present(Some(window));
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG: &[u8] = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00\x01\x00\x00";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";
    const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00";
    const WEBP: &[u8] = b"RIFF\x00\x00\x00\x00WEBPVP8 ";
    const PDF: &[u8] = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n";
    const OLE: &[u8] = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1\x00\x00\x00\x00";
    const ZIP: &[u8] = b"PK\x03\x04\x14\x00\x00\x00\x08\x00";
    const ODT: &[u8] =
        b"PK\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x27\x00\x00\x00\x27\x00\x00\x00\x08\x00\x00\x00mimetypeapplication/vnd.oasis.opendocument.text";
    const MP3: &[u8] = b"ID3\x03\x00\x00\x00\x00\x00\x00";
    const OGG: &[u8] = b"OggS\x00\x02\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x01\x13OpusHead";
    const MP4: &[u8] = b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom";
    const M4A: &[u8] = b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00M4A isom";
    const WEBM: &[u8] = b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\xf7\x81\x01\x42\x82\x84webm";
    const GZIP: &[u8] = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\x03";
    const SEVEN_Z: &[u8] = b"7z\xbc\xaf\x27\x1c\x00\x04";
    const TEXT: &[u8] = "Olá, mundo\nsegunda linha\n".as_bytes();
    const ELF: &[u8] = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x00";
    const MZ: &[u8] = b"MZ\x90\x00\x03\x00\x00\x00\x04\x00\x00\x00\xff\xff";
    const SHELL: &[u8] = b"#!/bin/sh\necho hi\n";
    const HTML: &[u8] = b"<!DOCTYPE html><html><body>x</body></html>";
    const SVG: &[u8] = b"<?xml version=\"1.0\"?><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
    const DESKTOP: &[u8] = b"[Desktop Entry]\nType=Application\nExec=sh\n";
    const TIFF: &[u8] = b"II*\x00\x08\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
    const CR2: &[u8] = b"II*\x00\x10\x00\x00\x00CR\x02\x00\x00\x00\x00\x00";
    const BMP: &[u8] = b"BM\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
    const AVIF: &[u8] = b"\x00\x00\x00\x1cftypavif\x00\x00\x00\x00avifmif1miaf";
    const HEIC: &[u8] = b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00mif1heic";
    const JXL: &[u8] = b"\xff\x0a\x00\x00\x00\x00\x00\x00";
    const MKV: &[u8] = b"\x1a\x45\xdf\xa3\x93\x42\x82\x88matroska";
    const ASF: &[u8] = b"\x30\x26\xb2\x75\x8e\x66\xcf\x11\xa6\xd9\x00\xaa\x00\x62\xce\x6c\x00\x00";
    const RAR: &[u8] = b"Rar!\x1a\x07\x01\x00\x00\x00\x00\x00";
    const EPUB: &[u8] = b"PK\x03\x04\x14\x00\x00\x00\x00\x00\x00\x00!\x00oa\xab,\x14\x00\x00\x00\x14\x00\x00\x00\x08\x00\x00\x00mimetypeapplication/epub+zip";
    const RTF: &[u8] = b"{\\rtf1\\ansi hello}";
    const EPS: &[u8] = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 10 10\n";
    const M3U: &[u8] = b"#EXTM3U\n#EXTINF:1,x\nhttp://example.invalid/a.mp3\n";
    const PLS: &[u8] = b"[playlist]\nFile1=http://example.invalid/a\nNumberOfEntries=1\n";
    const ASX: &[u8] =
        b"<asx version=\"3.0\"><entry><ref href=\"http://example.invalid/a\"/></entry></asx>";
    const RANDOM: &[u8] = &[0x00, 0x01, 0x02, 0x03, 0xfe, 0x80, 0x81, 0x00, 0x7f, 0x13];

    #[test]
    fn passive_documents_open_without_asking() {
        for (name, declared, head) in [
            ("photo.jpg", "image/jpeg", JPEG),
            ("photo.png", "image/png", PNG),
            ("anim.gif", "image/gif", GIF),
            ("sticker.webp", "image/webp", WEBP),
            ("report.pdf", "application/pdf", PDF),
            ("letter.odt", "application/vnd.oasis.opendocument.text", ODT),
            (
                "letter.docx",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                ZIP,
            ),
            (
                "sheet.xlsx",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                ZIP,
            ),
            (
                "slides.pptx",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                ZIP,
            ),
            ("legacy.doc", "application/msword", OLE),
            ("legacy.xls", "application/vnd.ms-excel", OLE),
            ("legacy.ppt", "application/vnd.ms-powerpoint", OLE),
            ("song.mp3", "audio/mpeg", MP3),
            ("voice.ogg", "audio/ogg; codecs=opus", OGG),
            ("voice.m4a", "audio/mp4", M4A),
            ("clip.mp4", "video/mp4", MP4),
            ("clip.webm", "video/webm", WEBM),
            ("notes.txt", "text/plain", TEXT),
            ("notes.md", "text/markdown", b"# Title\n\n- item\n"),
            ("table.csv", "text/csv", b"a,b,c\n1,2,3\n"),
            ("empty.txt", "text/plain", b""),
            ("archive.zip", "application/zip", ZIP),
            ("archive.tar.gz", "application/gzip", GZIP),
            ("archive.gz", "application/gzip", GZIP),
            ("archive.7z", "application/x-7z-compressed", SEVEN_Z),
            ("no-declared-type.pdf", "", PDF),
            ("photo.bmp", "image/bmp", BMP),
            ("photo.tiff", "image/tiff", TIFF),
            ("photo.avif", "image/avif", AVIF),
            ("photo.heic", "image/heic", HEIC),
            ("photo.jxl", "image/jxl", JXL),
            ("photo.dng", "image/x-adobe-dng", TIFF),
            ("photo.cr2", "image/x-canon-cr2", CR2),
            ("photo.nef", "image/x-nikon-nef", TIFF),
            ("photo.arw", "image/x-sony-arw", TIFF),
            ("clip.mkv", "video/x-matroska", MKV),
            ("clip.wmv", "video/x-ms-wmv", ASF),
            ("song.wma", "audio/x-ms-wma", ASF),
            ("archive.rar", "application/vnd.rar", RAR),
            ("archive.rar", "application/x-rar-compressed", RAR),
            ("book.epub", "application/epub+zip", EPUB),
            ("letter.rtf", "application/rtf", RTF),
            ("letter.rtf", "text/rtf", RTF),
        ] {
            assert!(!needs_confirmation(name, declared, head), "{name}");
        }
    }

    #[test]
    fn tar_archives_open_without_asking() {
        let mut tar = vec![0u8; 512];
        tar[..5].copy_from_slice(b"x.txt");
        tar[257..263].copy_from_slice(b"ustar\0");
        assert!(!needs_confirmation(
            "archive.tar",
            "application/x-tar",
            &tar
        ));
    }

    #[test]
    fn active_or_unknown_names_ask() {
        for name in [
            "unknown.bin",
            "page.xhtml",
            "launch.jnlp",
            "shortcut.lnk",
            "settings.reg",
            "script.vbs",
            "script.bat",
            "script.cmd",
            "app.hta",
            "help.chm",
            "script.py",
            "script.pl",
            "install.sh",
            "app.flatpak",
            "app.flatpakref",
            "repo.flatpakrepo",
            "package.rpm",
            "package.deb",
            "app.AppImage",
            "page.html",
            "drawing.svg",
            "program.exe",
            "package.msi",
            "tool.jar",
            "setup.desktop",
            "macro.docm",
            "data.json",
        ] {
            assert!(needs_confirmation(name, "", TEXT), "{name}");
            assert!(needs_confirmation(name, "", RANDOM), "{name} with binary");
        }
    }

    #[test]
    fn content_that_disagrees_with_a_passive_name_asks() {
        for (name, head) in [
            ("report.pdf", ELF),
            ("report.pdf", MZ),
            ("notes.txt", SHELL),
            ("notes.txt", HTML),
            ("photo.jpg", HTML),
            ("photo.jpg", SVG),
            ("notes.txt", DESKTOP),
            ("archive.zip", ELF),
            ("report.pdf", RANDOM),
            ("x.jpg", EPS),
            ("song.mp3", M3U),
        ] {
            assert!(needs_confirmation(name, "", head), "{name}");
        }
    }

    #[test]
    fn postscript_and_playlists_ask() {
        for (name, head) in [
            ("x.eps", EPS),
            ("x.jpg", EPS),
            ("list.m3u", M3U),
            ("list.m3u8", M3U),
            ("list.pls", PLS),
            ("list.asx", ASX),
        ] {
            assert!(needs_confirmation(name, "", head), "{name}");
        }
        for playlist in [
            "audio/x-mpegurl",
            "audio/mpegurl",
            "audio/x-scpls",
            "audio/x-ms-asx",
            "video/vnd.mpegurl",
            "video/x-ms-wvx",
            "image/x-eps",
            "image/x-gzeps",
            "image/x-bzeps",
        ] {
            assert!(!is_safe(playlist), "{playlist}");
        }
    }

    #[test]
    fn a_risky_declared_type_asks() {
        for declared in [
            "text/html",
            "application/xhtml+xml",
            "image/svg+xml",
            "application/x-desktop",
            "application/x-msdownload",
            "application/x-ms-dos-executable",
            "application/x-msi",
            "application/x-ms-shortcut",
            "application/x-executable",
            "application/vnd.microsoft.portable-executable",
            "TEXT/HTML; charset=utf-8",
        ] {
            assert!(
                needs_confirmation("report.pdf", declared, PDF),
                "{declared}"
            );
        }
    }

    #[test]
    fn an_unusual_declared_name_does_not_ask() {
        for (name, declared, head) in [
            ("archive.zip", "application/x-zip-compressed", ZIP),
            (
                "table.csv",
                "text/comma-separated-values",
                b"a,b\n1,2\n".as_slice(),
            ),
            ("archive.gz", "application/x-gzip", GZIP),
            ("report.pdf", "application/octet-stream", PDF),
            ("report.pdf", "application/x-unknown-sender-type", PDF),
            ("report.pdf", "text/plain", PDF),
            ("notes.txt", "application/json", TEXT),
        ] {
            assert!(
                !needs_confirmation(name, declared, head),
                "{name} {declared}"
            );
        }
    }

    #[test]
    fn type_families_are_matched_exactly() {
        assert!(is_safe("IMAGE/PNG"));
        assert!(!is_safe("image/x-icon"));
        assert!(!is_safe("image/x-xpixmap"));
        assert!(is_safe("video/mp4"));
        assert!(is_safe("audio/x-ms-wma"));
        assert!(is_safe("audio/ogg; codecs=opus"));
        assert!(!is_safe("image/svg+xml"));
        assert!(!is_safe("image/svg+xml-compressed"));
        assert!(!is_safe("image/"));
        assert!(!is_safe("audio/"));
        assert!(!is_safe("text/x-python"));
        assert!(!is_safe("text/plainx"));
        assert!(!is_safe("application/x-extension-html"));
        assert!(!is_safe("application/x-wine-extension-msp"));
        assert!(!is_safe("application/octet-stream"));
        assert!(!is_safe(""));
    }

    #[test]
    fn sniffing_reads_the_file_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let elf = dir.path().join("report.pdf");
        std::fs::write(&elf, ELF).unwrap();
        assert!(path_needs_confirmation(&elf, "application/pdf"));
        let pdf = dir.path().join("report-2.pdf");
        std::fs::write(&pdf, PDF).unwrap();
        assert!(!path_needs_confirmation(&pdf, "application/pdf"));
        assert!(path_needs_confirmation(
            &dir.path().join("missing.pdf"),
            "application/pdf"
        ));
    }

    #[test]
    fn displayed_names_drop_controls_and_direction_overrides() {
        assert_eq!(displayable("invoice\u{202e}fdp.exe"), "invoicefdp.exe");
        assert_eq!(displayable("a\u{2066}b\u{2069}\u{200f}c\n.pdf"), "abc.pdf");
        assert_eq!(
            displayable("in\u{200d}voi\u{feff}ce\u{00ad}\u{2028}\u{2029}.pdf"),
            "invoice.pdf"
        );
        assert_eq!(displayable("a\u{e0001}b\u{061c}.pdf"), "ab.pdf");
        assert_eq!(displayable("relatório final.pdf"), "relatório final.pdf");
    }
}
