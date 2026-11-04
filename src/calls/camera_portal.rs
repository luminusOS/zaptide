//! Camera access through the XDG Camera portal (`org.freedesktop.portal.Camera`),
//! by way of ashpd.
//!
//! A sandboxed app cannot open `/dev/video*`. The portal asks the person once,
//! then hands out a connection to PipeWire that only shows camera nodes. The
//! calls here block, so they run on the threads that already open call devices,
//! never on the GTK main thread.

use std::ffi::OsStr;
use std::future::Future;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::time::Duration;

use ashpd::desktop::ResponseError;
use ashpd::desktop::camera::Camera;
use ashpd::{Error as PortalFailure, PortalError};
use gstreamer as gst;
use gstreamer::prelude::*;

use super::MediaError;

/// Calls that answer at once; a portal that takes longer is treated as absent.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// The permission prompt waits for a person, so it gets much longer.
pub(super) const PROMPT_TIMEOUT: Duration = Duration::from_secs(60);

/// How a camera is reached on this system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// Inside a sandbox the portal is the only way to a camera.
    Portal,
    /// Outside one, V4L2 first; the portal covers cameras only PipeWire sees.
    DirectThenPortal,
}

pub(super) fn route(sandboxed: bool) -> Route {
    if sandboxed {
        Route::Portal
    } else {
        Route::DirectThenPortal
    }
}

fn is_sandboxed(flatpak_info_exists: bool, flatpak_id: Option<&OsStr>) -> bool {
    flatpak_info_exists || flatpak_id.is_some_and(|id| !id.is_empty())
}

pub(super) fn sandboxed() -> bool {
    is_sandboxed(
        Path::new("/.flatpak-info").exists(),
        std::env::var_os("FLATPAK_ID").as_deref(),
    )
}

/// A portal that is not there is not worth telling someone who has a camera
/// that only the direct route could have opened: report no camera instead.
pub(super) fn fallback_error(error: MediaError) -> MediaError {
    match error {
        MediaError::CameraPortal => MediaError::NoCamera,
        other => other,
    }
}

/// Maps a portal failure. The portal reports a refused prompt and a dismissed
/// one with the same response, so both read as denied.
fn media_error(error: &PortalFailure) -> MediaError {
    log::debug!("camera portal call failed: {error}");
    match error {
        PortalFailure::Response(ResponseError::Cancelled)
        | PortalFailure::Portal(PortalError::NotAllowed(_) | PortalError::Cancelled(_)) => {
            MediaError::CameraDenied
        }
        _ => MediaError::CameraPortal,
    }
}

async fn within<T>(
    limit: Duration,
    on_timeout: MediaError,
    call: impl Future<Output = Result<T, PortalFailure>>,
) -> Result<T, MediaError> {
    match tokio::time::timeout(limit, call).await {
        Ok(result) => result.map_err(|error| media_error(&error)),
        Err(_) => Err(on_timeout),
    }
}

async fn open_remote(prompt_timeout: Duration) -> Result<OwnedFd, MediaError> {
    let camera = within(CALL_TIMEOUT, MediaError::CameraPortal, Camera::new()).await?;
    if !within(CALL_TIMEOUT, MediaError::CameraPortal, camera.is_present()).await? {
        return Err(MediaError::NoCamera);
    }
    let request = within(
        prompt_timeout,
        MediaError::CameraTimedOut,
        camera.request_access(Default::default()),
    )
    .await?;
    request.response().map_err(|error| media_error(&error))?;
    within(
        CALL_TIMEOUT,
        MediaError::CameraPortal,
        camera.open_pipe_wire_remote(Default::default()),
    )
    .await
}

/// A camera node as the PipeWire device provider lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Node {
    class: String,
    serial: u64,
}

/// PipeWire's marker for an object without a serial.
const NO_SERIAL: u64 = u32::MAX as u64;

/// The first video source. The provider lists them by session priority, so
/// that is the camera the desktop prefers.
fn pick_camera(nodes: &[Node]) -> Result<u64, MediaError> {
    nodes
        .iter()
        .find(|node| node.class.starts_with("Video/Source") && node.serial != NO_SERIAL)
        .map(|node| node.serial)
        .ok_or(MediaError::NoCamera)
}

fn camera_node(remote: &OwnedFd) -> Result<u64, MediaError> {
    let provider = gst::DeviceProviderFactory::by_name("pipewiredeviceprovider")
        .ok_or(MediaError::NoCamera)?;
    provider.set_property("fd", remote.as_raw_fd());
    let nodes: Vec<Node> = provider
        .devices()
        .iter()
        .filter(|device| device.find_property("serial").is_some())
        .map(|device| Node {
            class: device.device_class().to_string(),
            serial: device.property::<u64>("serial"),
        })
        .collect();
    pick_camera(&nodes)
}

fn source_description(fd: RawFd, node: u64, reports_disconnect: bool) -> String {
    let mut source = format!("pipewiresrc fd={fd} target-object={node}");
    if reports_disconnect {
        source.push_str(" on-disconnect=error");
    }
    source
}

/// Whether `pipewiresrc` can report a removed camera as an error. Without it
/// an unplugged camera would only stop producing pictures.
fn reports_disconnect() -> bool {
    gst::ElementFactory::make("pipewiresrc")
        .build()
        .is_ok_and(|source| source.find_property("on-disconnect").is_some())
}

fn plugin_present() -> bool {
    gst::init().is_ok()
        && gst::ElementFactory::find("pipewiresrc").is_some()
        && gst::DeviceProviderFactory::find("pipewiredeviceprovider").is_some()
}

/// A PipeWire connection that sees the camera, and the node to read. The
/// descriptor must stay open as long as a pipeline reads from it.
#[derive(Debug)]
pub(super) struct Remote {
    fd: OwnedFd,
    node: u64,
}

impl Remote {
    pub(super) fn source(&self) -> String {
        source_description(self.fd.as_raw_fd(), self.node, reports_disconnect())
    }

    pub(super) fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

/// Asks the portal for the camera, waiting up to `prompt_timeout` for the
/// person to answer, and returns the PipeWire connection to read it from.
/// Checks for a camera first, so a machine without one never shows a prompt.
pub(super) fn open(prompt_timeout: Duration) -> Result<Remote, MediaError> {
    if !plugin_present() {
        log::warn!("the GStreamer PipeWire plugin is missing");
        return Err(MediaError::NoCamera);
    }
    let fd = crate::portal::run(open_remote(prompt_timeout)).ok_or(MediaError::CameraPortal)??;
    let node = camera_node(&fd)?;
    Ok(Remote { fd, node })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn node(class: &str, serial: u64) -> Node {
        Node {
            class: class.to_owned(),
            serial,
        }
    }

    #[test]
    fn a_flatpak_is_sandboxed_by_its_info_file_or_its_id() {
        assert!(is_sandboxed(true, None));
        assert!(is_sandboxed(
            false,
            Some(OsStr::new("dev.luminusos.ZapTide"))
        ));
        assert!(!is_sandboxed(false, None));
        assert!(!is_sandboxed(false, Some(&OsString::new())));
    }

    #[test]
    fn only_a_sandbox_goes_straight_to_the_portal() {
        assert_eq!(route(true), Route::Portal);
        assert_eq!(route(false), Route::DirectThenPortal);
    }

    #[test]
    fn a_missing_portal_after_the_direct_route_failed_means_no_camera() {
        assert_eq!(
            fallback_error(MediaError::CameraPortal),
            MediaError::NoCamera
        );
        for kept in [
            MediaError::CameraDenied,
            MediaError::CameraTimedOut,
            MediaError::NoCamera,
        ] {
            assert_eq!(fallback_error(kept.clone()), kept);
        }
    }

    #[test]
    fn the_first_video_source_is_the_camera() {
        let nodes = [
            node("Audio/Source", 10),
            node("Video/Source", 21),
            node("Video/Source", 22),
        ];
        assert_eq!(pick_camera(&nodes), Ok(21));
        assert_eq!(
            pick_camera(&[node("Video/Source/Virtual", 5)]),
            Ok(5),
            "a virtual camera is still a camera"
        );
    }

    #[test]
    fn nothing_that_is_not_a_camera_is_picked() {
        assert_eq!(pick_camera(&[]), Err(MediaError::NoCamera));
        let others = [
            node("Audio/Source", 1),
            node("Video/Sink", 2),
            node("Video/Source", NO_SERIAL),
        ];
        assert_eq!(pick_camera(&others), Err(MediaError::NoCamera));
    }

    #[test]
    fn the_source_names_the_connection_and_the_node() {
        assert_eq!(
            source_description(7, 42, false),
            "pipewiresrc fd=7 target-object=42"
        );
        assert_eq!(
            source_description(7, 42, true),
            "pipewiresrc fd=7 target-object=42 on-disconnect=error"
        );
    }

    #[test]
    fn a_refused_or_dismissed_prompt_is_denied() {
        let cancelled = PortalFailure::Response(ResponseError::Cancelled);
        assert_eq!(media_error(&cancelled), MediaError::CameraDenied);
        let not_allowed = PortalFailure::Portal(PortalError::NotAllowed("no".into()));
        assert_eq!(media_error(&not_allowed), MediaError::CameraDenied);
        let portal_cancelled = PortalFailure::Portal(PortalError::Cancelled("no".into()));
        assert_eq!(media_error(&portal_cancelled), MediaError::CameraDenied);
    }

    #[test]
    fn any_other_failure_is_the_portal() {
        let others = [
            PortalFailure::Response(ResponseError::Other),
            PortalFailure::Portal(PortalError::Failed("broken".into())),
            PortalFailure::Portal(PortalError::NotFound("gone".into())),
            PortalFailure::NoResponse,
            PortalFailure::RequiresVersion(2, 1),
            PortalFailure::IO(std::io::Error::other("closed")),
        ];
        for other in &others {
            assert_eq!(media_error(other), MediaError::CameraPortal, "{other}");
        }
    }

    fn run_within<T: Send + 'static>(
        limit: Duration,
        call: impl Future<Output = Result<T, PortalFailure>> + Send + 'static,
    ) -> Result<T, MediaError> {
        crate::portal::run(within(limit, MediaError::CameraTimedOut, call))
            .expect("the portal runtime finishes")
    }

    #[test]
    fn a_call_that_never_answers_times_out() {
        let started = std::time::Instant::now();
        let result = run_within(
            Duration::from_millis(60),
            std::future::pending::<Result<(), _>>(),
        );
        assert_eq!(result, Err(MediaError::CameraTimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_call_that_answers_in_time_keeps_its_result_or_its_mapped_error() {
        assert_eq!(run_within(Duration::from_secs(30), async { Ok(5) }), Ok(5));
        let refused = run_within(Duration::from_secs(30), async {
            Err::<(), _>(PortalFailure::Response(ResponseError::Cancelled))
        });
        assert_eq!(refused, Err(MediaError::CameraDenied));
    }

    #[test]
    #[ignore = "talks to the session's camera portal"]
    fn the_session_portal_answers_whether_a_camera_is_present() {
        let present = crate::portal::run(async {
            within(CALL_TIMEOUT, MediaError::CameraPortal, async {
                Camera::new().await?.is_present().await
            })
            .await
        })
        .expect("the portal runtime finishes");
        match present {
            Ok(present) => eprintln!("the portal reports a camera: {present}"),
            Err(MediaError::CameraPortal) => eprintln!("skipped: the session has no camera portal"),
            Err(error) => panic!("unexpected: {error}"),
        }
    }

    #[test]
    #[ignore = "opens the real camera for a moment; never saves a picture"]
    fn the_session_portal_opens_a_camera_that_encodes() {
        use crate::calls::video::{Camera as VideoCamera, available_encoder};
        let Some(encoder) = available_encoder() else {
            eprintln!("skipped: no H.264 encoder is installed");
            return;
        };
        let remote = match open(Duration::from_secs(45)) {
            Ok(remote) => remote,
            Err(MediaError::NoCamera) => {
                eprintln!("skipped: the portal reports no camera");
                return;
            }
            Err(MediaError::CameraPortal) => {
                eprintln!("skipped: the session has no camera portal");
                return;
            }
            Err(error) => panic!("the portal did not open the camera: {error}"),
        };
        let slots = std::sync::Arc::new(crate::calls::VideoSlots::new(|| {}));
        let camera =
            VideoCamera::start_remote(encoder, remote, slots, crate::calls::ignore_faults())
                .expect("the camera starts through the portal");
        let unit = crate::calls::recv_within(&camera.frames(), Duration::from_secs(10))
            .expect("the camera produces a picture");
        assert!(!unit.is_empty());
        std::thread::sleep(Duration::from_secs(1));
    }
}
