//! GTK/GIO desktop operations for the native shell.
//!
//! Construct and call [`NativePortals`] on the GTK thread. GTK dialogs and
//! clipboard transfers are asynchronous; this module performs no synchronous
//! filesystem or network access. File chooser results are returned as `gio::File`
//! handles so callers can schedule their own asynchronous I/O away from GTK.

use std::{cell::RefCell, collections::HashMap, io::Cursor, path::Path, rc::Rc};

use gtk::{gdk, gio, prelude::*};
use relm4::gtk;

const MAX_CLIPBOARD_IMAGE_DIMENSION: i32 = 16_384;
const MAX_CLIPBOARD_IMAGE_PIXELS: u64 = 100_000_000;
const MAX_CLIPBOARD_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const CLIPBOARD_IMAGE_READ_CHUNK: usize = 64 * 1024;
const CLIPBOARD_IMAGE_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
    "image/tiff",
];

/// Opaque identifier for a portal or clipboard operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RequestId(u64);

/// Error reported by an asynchronous desktop operation.
///
/// The underlying GLib error is intentionally not retained: its text may include
/// a private path or URI and must not accidentally reach logs or user-visible UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortalError {
    Failed,
}

/// Invalid input rejected before invoking a desktop service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationError {
    Empty,
    TooLong,
    InvalidSyntax,
    UnsupportedScheme,
    InvalidAuthority,
    InvalidPath,
    RequestIdsExhausted,
}

/// Result of validating a URI for explicit user-initiated external opening.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedUri(String);

impl ValidatedUri {
    /// The normalized URI string. Scheme is lowercase; other components are retained.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Explicit caller assertion that opening a URI is the immediate result of a
/// user action (for example, activating a link or menu item).
///
/// Construct this token only inside the handler for that action; do not retain
/// it or use it for background, startup, or indirectly triggered navigation.
#[derive(Clone, Copy, Debug)]
pub struct UserAction {
    _private: (),
}

impl UserAction {
    /// Marks an operation as directly initiated by the current user action.
    pub fn for_explicit_user_action() -> Self {
        Self { _private: () }
    }
}

#[derive(Default)]
struct Requests {
    next_id: u64,
    pending: HashMap<RequestId, gio::Cancellable>,
}

impl Requests {
    fn start(&mut self) -> Option<(RequestId, gio::Cancellable)> {
        self.next_id = self.next_id.checked_add(1)?;
        let id = RequestId(self.next_id);
        let cancellable = gio::Cancellable::new();
        self.pending.insert(id, cancellable.clone());
        Some((id, cancellable))
    }

    fn finish(&mut self, id: RequestId) -> bool {
        self.pending.remove(&id).is_some()
    }

    fn cancel(&mut self, id: RequestId) -> bool {
        let Some(cancellable) = self.pending.remove(&id) else {
            return false;
        };
        cancellable.cancel();
        true
    }

    fn cancel_all(&mut self) {
        for (_, cancellable) in self.pending.drain() {
            cancellable.cancel();
        }
    }
}

/// Native, portal-backed file and clipboard operations.
///
/// `Rc` ownership deliberately makes this type non-`Send`. All methods must be
/// called on GTK's main thread. Cancellation invalidates a request immediately;
/// its eventual GIO completion is ignored and its callback is dropped.
#[derive(Clone, Default)]
pub struct NativePortals {
    requests: Rc<RefCell<Requests>>,
}

impl NativePortals {
    /// Starts asynchronous multi-file selection using `GtkFileDialog`.
    ///
    /// GTK routes this native chooser through `org.freedesktop.portal.FileChooser`
    /// when available. Returned files are restricted to absolute local file paths.
    pub fn open_files(
        &self,
        parent: Option<&impl IsA<gtk::Window>>,
        title: &str,
        complete: impl FnOnce(Result<Vec<gio::File>, PortalError>) + 'static,
    ) -> Option<RequestId> {
        let dialog = gtk::FileDialog::builder().title(title).build();
        let (id, cancellable) = self.requests.borrow_mut().start()?;
        let requests = self.requests.clone();
        dialog.open_multiple(parent, Some(&cancellable), move |result| {
            if !requests.borrow_mut().finish(id) {
                return;
            }
            let result = result
                .map_err(|_| PortalError::Failed)
                .and_then(|files| collect_local_files(&files));
            complete(result);
        });
        Some(id)
    }

    /// Selects a save destination and copies an existing local attachment
    /// asynchronously. The request ID remains cancellable through the copy.
    pub fn save_copy(
        &self,
        parent: Option<&impl IsA<gtk::Window>>,
        source: &gio::File,
        title: &str,
        suggested_name: &str,
        complete: impl FnOnce(Result<(), PortalError>) + 'static,
    ) -> Option<RequestId> {
        if !valid_filename(suggested_name) {
            return None;
        }
        let dialog = gtk::FileDialog::builder()
            .title(title)
            .initial_name(suggested_name)
            .build();
        let (id, cancellable) = self.requests.borrow_mut().start()?;
        let requests = self.requests.clone();
        let source = source.clone();
        let copy_cancellable = cancellable.clone();
        dialog.save(parent, Some(&cancellable), move |result| {
            if !requests.borrow().pending.contains_key(&id) {
                return;
            }
            let Ok(destination) = result else {
                if requests.borrow_mut().finish(id) {
                    complete(Err(PortalError::Failed));
                }
                return;
            };
            source.copy_async(
                &destination,
                gio::FileCopyFlags::OVERWRITE,
                gtk::glib::Priority::DEFAULT,
                Some(&copy_cancellable),
                None,
                move |result| {
                    if requests.borrow_mut().finish(id) {
                        complete(result.map_err(|_| PortalError::Failed));
                    }
                },
            );
        });
        Some(id)
    }

    /// Validates and asynchronously opens an HTTP(S) or mailto URI in its default app.
    ///
    /// `user_action` must be created and passed immediately from the handler for
    /// an explicit user action. Never use this operation for background opens.
    pub fn open_uri(
        &self,
        _user_action: UserAction,
        uri: &str,
        complete: impl FnOnce(Result<(), PortalError>) + 'static,
    ) -> Result<RequestId, ValidationError> {
        let uri = validate_uri(uri)?;
        let Some((id, cancellable)) = self.requests.borrow_mut().start() else {
            return Err(ValidationError::RequestIdsExhausted);
        };
        let requests = self.requests.clone();
        gio::AppInfo::launch_default_for_uri_async(
            uri.as_str(),
            None::<&gio::AppLaunchContext>,
            Some(&cancellable),
            move |result| {
                if !requests.borrow_mut().finish(id) {
                    return;
                }
                complete(result.map_err(|_| PortalError::Failed));
            },
        );
        Ok(id)
    }

    /// Opens an application-owned local directory in the user's default file manager.
    ///
    /// Unlike arbitrary URI opening, this accepts only absolute local paths supplied
    /// by the application, such as its own themes directory.
    pub fn open_folder(
        &self,
        _user_action: UserAction,
        path: &std::path::Path,
        complete: impl FnOnce(Result<(), PortalError>) + 'static,
    ) -> Result<RequestId, ValidationError> {
        let uri = local_folder_uri(path)?;
        let Some((id, cancellable)) = self.requests.borrow_mut().start() else {
            return Err(ValidationError::RequestIdsExhausted);
        };
        let requests = self.requests.clone();
        gio::AppInfo::launch_default_for_uri_async(
            &uri,
            None::<&gio::AppLaunchContext>,
            Some(&cancellable),
            move |result| {
                if !requests.borrow_mut().finish(id) {
                    return;
                }
                complete(result.map_err(|_| PortalError::Failed));
            },
        );
        Ok(id)
    }

    /// Asynchronously reads an image with bounded input and validates its dimensions
    /// before GDK decodes it into a texture.
    pub fn read_clipboard_image(
        &self,
        clipboard: &gdk::Clipboard,
        complete: impl FnOnce(Result<Option<gdk::Texture>, PortalError>) + 'static,
    ) -> Option<RequestId> {
        let (id, cancellable) = self.requests.borrow_mut().start()?;
        let requests = self.requests.clone();
        let callback_cancellable = cancellable.clone();
        clipboard.read_async(
            CLIPBOARD_IMAGE_MIME_TYPES,
            gtk::glib::Priority::DEFAULT,
            Some(&cancellable),
            move |result| {
                if !requests.borrow().pending.contains_key(&id) {
                    return;
                }
                match result {
                    Err(_) => {
                        if requests.borrow_mut().finish(id) {
                            complete(Err(PortalError::Failed));
                        }
                    }
                    Ok((stream, mime_type)) => {
                        if validate_clipboard_image_mime(mime_type.as_str()).is_err() {
                            if requests.borrow_mut().finish(id) {
                                complete(Err(PortalError::Failed));
                            }
                            return;
                        }
                        let pending_requests = requests.clone();
                        read_bounded_stream(stream, callback_cancellable, move |read_result| {
                            if !pending_requests.borrow_mut().finish(id) {
                                return;
                            }
                            complete(read_result.and_then(|bytes| {
                                decode_clipboard_texture(&bytes, mime_type.as_str())
                                    .map(Some)
                                    .ok_or(PortalError::Failed)
                            }));
                        });
                    }
                }
            },
        );
        Some(id)
    }

    pub fn write_clipboard_text(clipboard: &gdk::Clipboard, text: &str) {
        clipboard.set_text(text);
    }

    /// Cancels one pending request. Returns false if request already completed or unknown.
    pub fn cancel(&self, id: RequestId) -> bool {
        self.requests.borrow_mut().cancel(id)
    }

    /// Cancels and invalidates all requests. Call during application shutdown.
    pub fn cancel_all(&self) {
        self.requests.borrow_mut().cancel_all();
    }
}

fn local_folder_uri(path: &std::path::Path) -> Result<String, ValidationError> {
    if !path.is_absolute() {
        return Err(ValidationError::InvalidPath);
    }
    Ok(gio::File::for_path(path).uri().to_string())
}

/// Applies URI scheme, authority, percent-encoding, and path checks without I/O.
pub fn validate_uri(input: &str) -> Result<ValidatedUri, ValidationError> {
    if input.is_empty() {
        return Err(ValidationError::Empty);
    }
    if input.len() > 8_192 {
        return Err(ValidationError::TooLong);
    }
    if input
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '\\')
        || !valid_percent_escapes(input)
        || has_encoded_control(input)
    {
        return Err(ValidationError::InvalidSyntax);
    }

    let (scheme, rest) = input
        .split_once(':')
        .ok_or(ValidationError::InvalidSyntax)?;
    if !valid_scheme(scheme) {
        return Err(ValidationError::InvalidSyntax);
    }
    let scheme = scheme.to_ascii_lowercase();
    match scheme.as_str() {
        "http" | "https" => {
            let authority_and_path = rest
                .strip_prefix("//")
                .ok_or(ValidationError::InvalidAuthority)?;
            let authority_end = authority_and_path
                .find(['/', '?', '#'])
                .unwrap_or(authority_and_path.len());
            let authority = &authority_and_path[..authority_end];
            let path_and_suffix = &authority_and_path[authority_end..];
            validate_authority(authority)?;
            validate_uri_path(path_and_suffix)?;
            Ok(ValidatedUri(format!("{scheme}:{rest}")))
        }
        "mailto" => {
            let address = rest.split(['?', '#']).next().unwrap_or_default();
            let (local, domain) = address
                .split_once('@')
                .ok_or(ValidationError::InvalidAuthority)?;
            if local.is_empty()
                || domain.is_empty()
                || address.matches('@').count() != 1
                || local.starts_with('.')
                || local.ends_with('.')
                || local.contains("..")
                || domain.starts_with('.')
                || domain.ends_with('.')
                || domain.contains("..")
            {
                return Err(ValidationError::InvalidAuthority);
            }
            validate_uri_path(rest)?;
            Ok(ValidatedUri(format!("{scheme}:{rest}")))
        }
        _ => Err(ValidationError::UnsupportedScheme),
    }
}

/// Validates a concrete MIME type (without parameters or wildcards).
pub fn validate_mime_type(input: &str) -> Result<(), ValidationError> {
    if input.is_empty() || input.len() > 127 {
        return Err(if input.is_empty() {
            ValidationError::Empty
        } else {
            ValidationError::TooLong
        });
    }
    let Some((major, minor)) = input.split_once('/') else {
        return Err(ValidationError::InvalidSyntax);
    };
    let valid_token = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#'
                            ..=b'\'' | b'*' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
                    )
            })
    };
    if valid_token(major) && valid_token(minor) && major != "*" && minor != "*" {
        Ok(())
    } else {
        Err(ValidationError::InvalidSyntax)
    }
}

fn supported_clipboard_image_mime(mime_type: &str) -> bool {
    CLIPBOARD_IMAGE_MIME_TYPES.contains(&mime_type)
}

fn validate_clipboard_image_mime(mime_type: &str) -> Result<(), ValidationError> {
    validate_mime_type(mime_type)?;
    if supported_clipboard_image_mime(mime_type) {
        Ok(())
    } else {
        Err(ValidationError::InvalidSyntax)
    }
}

fn read_bounded_stream(
    stream: gio::InputStream,
    cancellable: gio::Cancellable,
    complete: impl FnOnce(Result<Vec<u8>, PortalError>) + 'static,
) {
    type Completion = Box<dyn FnOnce(Result<Vec<u8>, PortalError>)>;

    fn read_next(
        stream: gio::InputStream,
        cancellable: gio::Cancellable,
        bytes: Vec<u8>,
        complete: Rc<RefCell<Option<Completion>>>,
    ) {
        let remaining = MAX_CLIPBOARD_IMAGE_BYTES.saturating_sub(bytes.len());
        let count = remaining.clamp(1, CLIPBOARD_IMAGE_READ_CHUNK);
        let read_stream = stream.clone();
        let read_cancellable = cancellable.clone();
        read_stream.read_bytes_async(
            count,
            gtk::glib::Priority::DEFAULT,
            Some(&read_cancellable),
            move |result| {
                let result = result.map_err(|_| PortalError::Failed);
                match result {
                    Err(error) => {
                        if let Some(complete) = complete.borrow_mut().take() {
                            complete(Err(error));
                        }
                    }
                    Ok(chunk) if chunk.is_empty() => {
                        if let Some(complete) = complete.borrow_mut().take() {
                            complete(Ok(bytes));
                        }
                    }
                    Ok(chunk) => {
                        let mut bytes = bytes;
                        match append_bounded_chunk(&mut bytes, chunk.as_ref()) {
                            Ok(()) => read_next(stream, cancellable, bytes, complete),
                            Err(error) => {
                                if let Some(complete) = complete.borrow_mut().take() {
                                    complete(Err(error));
                                }
                            }
                        }
                    }
                }
            },
        );
    }

    read_next(
        stream,
        cancellable,
        Vec::with_capacity(CLIPBOARD_IMAGE_READ_CHUNK),
        Rc::new(RefCell::new(Some(Box::new(complete)))),
    );
}

fn append_bounded_chunk(bytes: &mut Vec<u8>, chunk: &[u8]) -> Result<(), PortalError> {
    if chunk.len() > MAX_CLIPBOARD_IMAGE_BYTES.saturating_sub(bytes.len()) {
        return Err(PortalError::Failed);
    }
    bytes.extend_from_slice(chunk);
    Ok(())
}

fn decode_clipboard_texture(bytes: &[u8], mime_type: &str) -> Option<gdk::Texture> {
    let format = image::guess_format(bytes).ok()?;
    if image_mime_for_format(format)? != mime_type {
        return None;
    }
    let reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let (width, height) = reader.into_dimensions().ok()?;
    if width > MAX_CLIPBOARD_IMAGE_DIMENSION as u32
        || height > MAX_CLIPBOARD_IMAGE_DIMENSION as u32
        || u64::from(width) * u64::from(height) > MAX_CLIPBOARD_IMAGE_PIXELS
    {
        return None;
    }
    let bytes = gtk::glib::Bytes::from_owned(bytes.to_vec());
    gdk::Texture::from_bytes(&bytes).ok()
}

fn image_mime_for_format(format: image::ImageFormat) -> Option<&'static str> {
    match format {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::WebP => Some("image/webp"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::Bmp => Some("image/bmp"),
        image::ImageFormat::Tiff => Some("image/tiff"),
        _ => None,
    }
}

fn valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic())
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.'))
}

fn valid_percent_escapes(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return false;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    true
}

fn has_encoded_control(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.windows(3).any(|part| {
        part[0] == b'%'
            && u8::from_str_radix(std::str::from_utf8(&part[1..]).unwrap_or_default(), 16)
                .is_ok_and(|byte| byte < 0x20 || byte == 0x7f)
    })
}

fn validate_authority(authority: &str) -> Result<(), ValidationError> {
    if authority.is_empty() || authority.contains('@') || authority.contains('%') {
        return Err(ValidationError::InvalidAuthority);
    }
    let (host, port, is_ipv6) = if let Some(bracketed) = authority.strip_prefix('[') {
        let close = bracketed
            .find(']')
            .ok_or(ValidationError::InvalidAuthority)?;
        let host = &bracketed[..close];
        host.parse::<std::net::Ipv6Addr>()
            .map_err(|_| ValidationError::InvalidAuthority)?;
        let suffix = &bracketed[close + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(
                suffix
                    .strip_prefix(':')
                    .ok_or(ValidationError::InvalidAuthority)?,
            )
        };
        (host, port, true)
    } else {
        let mut split = authority.rsplitn(2, ':');
        let last = split.next().unwrap_or_default();
        match split.next() {
            Some(host) => (host, Some(last), false),
            None => (last, None, false),
        }
    };
    if host.is_empty()
        || (!is_ipv6
            && !host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')))
        || (!is_ipv6
            && (host.starts_with('.')
                || host.ends_with('.')
                || host.contains("..")
                || host.split('.').any(|label| {
                    label.starts_with('-') || label.ends_with('-') || label.len() > 63
                })))
    {
        return Err(ValidationError::InvalidAuthority);
    }
    if let Some(port) = port
        && (port.is_empty()
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || !matches!(port.parse::<u16>(), Ok(1..=u16::MAX)))
    {
        return Err(ValidationError::InvalidAuthority);
    }
    Ok(())
}

fn validate_uri_path(path_and_suffix: &str) -> Result<(), ValidationError> {
    let path = path_and_suffix.split(['?', '#']).next().unwrap_or_default();
    for segment in path.split('/') {
        let mut decoded = segment.to_owned();
        // Reject traversal after repeated decoding too: consumers may decode
        // an escaped percent before interpreting path segments.
        for _ in 0..=segment.len() {
            let next = percent_decode_ascii(&decoded)?;
            if next == decoded {
                break;
            }
            decoded = next;
        }
        if decoded == "."
            || decoded == ".."
            || decoded.contains(['/', '\\'])
            || decoded.chars().any(char::is_control)
        {
            return Err(ValidationError::InvalidPath);
        }
    }
    Ok(())
}

fn percent_decode_ascii(segment: &str) -> Result<String, ValidationError> {
    let bytes = segment.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(ValidationError::InvalidSyntax);
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .map_err(|_| ValidationError::InvalidSyntax)?;
            output.push(u8::from_str_radix(hex, 16).map_err(|_| ValidationError::InvalidSyntax)?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).map_err(|_| ValidationError::InvalidSyntax)
}

fn collect_local_files(files: &gio::ListModel) -> Result<Vec<gio::File>, PortalError> {
    let mut selected = Vec::with_capacity(files.n_items() as usize);
    for index in 0..files.n_items() {
        let file = files
            .item(index)
            .and_downcast::<gio::File>()
            .ok_or(PortalError::Failed)?;
        selected.push(validate_local_file(file)?);
    }
    Ok(selected)
}

fn validate_local_file(file: gio::File) -> Result<gio::File, PortalError> {
    let path = file.path().ok_or(PortalError::Failed)?;
    if file.uri_scheme().as_deref() != Some("file") || !valid_absolute_path(&path) {
        return Err(PortalError::Failed);
    }
    Ok(file)
}

fn valid_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|component| {
            matches!(
                component,
                std::path::Component::RootDir | std::path::Component::Normal(_)
            )
        })
}

fn valid_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name
            .chars()
            .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_policy_accepts_web_and_mail_links_only() {
        assert_eq!(
            validate_uri("HTTPS://example.org/a%20b").unwrap().as_str(),
            "https://example.org/a%20b"
        );
        assert_eq!(
            validate_uri("mailto:person@example.org").unwrap().as_str(),
            "mailto:person@example.org"
        );
        for input in [
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/plain,x",
            "//example.org",
            "https://user@example.org",
            "https://example.org/%2e%2e/private",
            "https://example.org/%0d%0aX:bad",
            "https://exa mple.org",
        ] {
            assert!(validate_uri(input).is_err(), "accepted {input}");
        }
    }

    #[test]
    fn theme_folder_open_accepts_only_absolute_local_paths() {
        assert_eq!(
            local_folder_uri(std::path::Path::new("relative/themes")),
            Err(ValidationError::InvalidPath)
        );
        let uri = local_folder_uri(std::path::Path::new("/tmp/zaptide themes"))
            .expect("absolute local path");
        assert!(uri.starts_with("file:///tmp/zaptide%20themes"));
    }

    #[test]
    fn uri_validation_rejects_bad_authorities_and_encoding() {
        for input in [
            "https://",
            "https://example.org:99999",
            "https://[not-ipv6]/",
            "https://example..org/",
            "https://example.org/%Q0",
            "https://example.org/%252e%252e/private",
            "https://example.org/%2f..%2fprivate",
            "https://example.org/%255c..%255cprivate",
            "https://user:pass@example.org/",
            "https://user%40name@example.org/",
            "https://-bad.example/",
            "https://exaｍple.org/",
        ] {
            assert!(validate_uri(input).is_err(), "accepted {input}");
        }
        assert_eq!(validate_uri("").unwrap_err(), ValidationError::Empty);
        assert_eq!(
            validate_uri("https://example.org/").unwrap().as_str(),
            "https://example.org/"
        );
        assert_eq!(
            validate_uri("https://[2001:db8::1]:443/").unwrap().as_str(),
            "https://[2001:db8::1]:443/"
        );
    }

    #[test]
    fn mime_types_require_concrete_ascii_tokens() {
        for mime in ["image/png", "application/vnd.example+json", "text/plain"] {
            assert_eq!(validate_mime_type(mime), Ok(()));
        }
        for mime in [
            "",
            "image",
            "/png",
            "image/",
            "image/*",
            "*/png",
            "image/png; charset=utf-8",
            "image/png\n",
            "imagé/png",
        ] {
            assert!(validate_mime_type(mime).is_err(), "accepted {mime:?}");
        }
    }

    #[test]
    fn clipboard_image_mime_policy_is_concrete_and_limited() {
        for mime in ["image/png", "image/jpeg", "image/webp", "image/gif"] {
            assert_eq!(validate_clipboard_image_mime(mime), Ok(()));
        }
        for mime in ["text/plain", "image/svg+xml"] {
            assert_eq!(
                validate_clipboard_image_mime(mime),
                Err(ValidationError::InvalidSyntax)
            );
        }
        assert_eq!(
            validate_clipboard_image_mime("image/png; charset=utf-8"),
            Err(ValidationError::InvalidSyntax)
        );
    }

    #[test]
    fn explicit_user_action_token_is_constructible_for_action_handlers() {
        let _user_action = UserAction::for_explicit_user_action();
    }

    #[test]
    fn clipboard_texture_requires_matching_mime_and_bounded_dimensions() {
        let mut png = Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 3)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let png = png.into_inner();
        let texture = decode_clipboard_texture(&png, "image/png").unwrap();
        assert_eq!((texture.width(), texture.height()), (2, 3));
        assert!(decode_clipboard_texture(&png, "image/jpeg").is_none());
        assert!(decode_clipboard_texture(b"not an image", "image/png").is_none());

        let mut oversized = Vec::new();
        {
            let mut encoder =
                png::Encoder::new(&mut oversized, MAX_CLIPBOARD_IMAGE_DIMENSION as u32 + 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header().unwrap();
        }
        assert!(decode_clipboard_texture(&oversized, "image/png").is_none());
    }

    #[test]
    fn clipboard_image_stream_limit_is_finite_and_chunked() {
        assert_eq!(CLIPBOARD_IMAGE_READ_CHUNK, 64 * 1024);
        assert_eq!(MAX_CLIPBOARD_IMAGE_BYTES, 32 * 1024 * 1024);

        let mut bytes = Vec::new();
        assert_eq!(append_bounded_chunk(&mut bytes, b"image"), Ok(()));
        assert_eq!(bytes, b"image");
        bytes.resize(MAX_CLIPBOARD_IMAGE_BYTES, 0);
        assert_eq!(
            append_bounded_chunk(&mut bytes, b"x"),
            Err(PortalError::Failed)
        );
        assert_eq!(bytes.len(), MAX_CLIPBOARD_IMAGE_BYTES);
    }

    #[test]
    fn path_and_name_validation_is_lexical_and_io_free() {
        assert!(valid_absolute_path(Path::new("/home/user/Downloads/file")));
        assert!(!valid_absolute_path(Path::new("relative/file")));
        assert!(!valid_absolute_path(Path::new("/home/../etc/passwd")));
        assert!(valid_filename("report.pdf"));
        for name in [
            "",
            ".",
            "..",
            "../escape",
            "folder/name",
            r"folder\name",
            "bad\nname",
        ] {
            assert!(!valid_filename(name), "accepted {name:?}");
        }
    }

    #[test]
    fn cancellation_invalidates_only_active_request() {
        let mut requests = Requests::default();
        let (first, first_cancellable) = requests.start().unwrap();
        let (second, _) = requests.start().unwrap();
        assert!(requests.cancel(first));
        assert!(first_cancellable.is_cancelled());
        assert!(!requests.finish(first));
        assert!(requests.finish(second));
        assert!(!requests.cancel(second));
    }

    #[test]
    fn chooser_request_id_cancels_only_its_own_cancellable() {
        let mut requests = Requests::default();
        let (chooser, chooser_cancellable) = requests.start().unwrap();
        let (other, other_cancellable) = requests.start().unwrap();

        assert!(requests.cancel(chooser));
        assert!(chooser_cancellable.is_cancelled());
        assert!(!other_cancellable.is_cancelled());
        assert!(!requests.finish(chooser));
        assert!(requests.finish(other));
    }

    #[test]
    fn cancel_all_invalidates_pending_ids() {
        let mut requests = Requests::default();
        let (first, cancellable) = requests.start().unwrap();
        requests.cancel_all();
        assert!(cancellable.is_cancelled());
        assert!(!requests.finish(first));
    }
}
