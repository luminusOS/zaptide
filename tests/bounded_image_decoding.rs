//! Remote images must not reach GDK's or gdk-pixbuf's own loaders, which
//! sniff the bytes and hand them to every gdk-pixbuf loader in-process (TIFF,
//! ICO, XPM, SVG...). They go through `native_media`'s bounded decoders.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const UNBOUNDED_LOADERS: [&str; 10] = [
    "Texture::from_bytes(",
    "Texture::from_filename(",
    "Texture::from_file(",
    "Pixbuf::from_",
    "Picture::for_filename(",
    "Picture::for_file(",
    "Image::from_file(",
    "set_from_file(",
    "set_filename(",
    "BytesIcon::new(",
];

/// Calls allowed per file, each loading bytes ZapTide produced itself or the
/// user chose on this computer.
const ALLOWED: [(&str, usize, &str); 6] = [
    (
        "src/application/linking.rs",
        1,
        "QR code PNG encoded locally",
    ),
    ("src/native_tray.rs", 1, "bundled tray icon SVG"),
    (
        "src/native_portals.rs",
        1,
        "clipboard image, format and size checked with the image crate first",
    ),
    (
        "src/application/profile_dialog.rs",
        1,
        "preview of a profile photo the user picked from disk",
    ),
    (
        "src/application/composer.rs",
        1,
        "preview of an attachment the user picked from disk",
    ),
    (
        "src/native_notifications.rs",
        1,
        "notification icon PNG that ZapTide encoded after a bounded decode",
    ),
];

fn rust_files(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn loader_calls(source: &str) -> usize {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .map(|line| {
            UNBOUNDED_LOADERS
                .iter()
                .map(|loader| line.matches(loader).count())
                .sum::<usize>()
        })
        .sum()
}

#[test]
fn image_loaders_are_only_used_for_trusted_local_images() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    let mut found = BTreeMap::new();
    for file in files {
        let calls = loader_calls(&std::fs::read_to_string(&file).unwrap());
        if calls > 0 {
            let relative = file
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            found.insert(relative, calls);
        }
    }
    let allowed: BTreeMap<String, usize> = ALLOWED
        .iter()
        .map(|(path, count, _)| ((*path).to_owned(), *count))
        .collect();
    assert_eq!(
        found, allowed,
        "decode remote images with native_media's bounded decoders, and keep ALLOWED current"
    );
}

#[test]
fn comment_lines_are_not_counted() {
    assert_eq!(
        loader_calls("// gdk::Texture::from_bytes(&bytes)\n    /// Pixbuf::from_file(path)\n"),
        0
    );
    assert_eq!(loader_calls("let t = gdk::Texture::from_bytes(&bytes);"), 1);
}
