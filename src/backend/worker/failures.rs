//! Classifies protocol failures by their typed cause instead of their text.

use whatsapp_rust::ErrorChainExt;

use crate::backend::DownloadFailure;

fn http_status(error: &anyhow::Error) -> Option<u16> {
    let cause: &(dyn std::error::Error + Send + Sync + 'static) = error.as_ref();
    cause.http_status()
}

/// Whether a failed download is worth asking the phone to re-upload: the
/// CDN no longer serves the file at that path.
pub(super) fn needs_reupload(error: &anyhow::Error) -> bool {
    matches!(http_status(error), Some(403 | 404 | 410))
}

/// The failure the bubble shows for a download that will not be retried.
pub(super) fn download_failure(error: &anyhow::Error) -> DownloadFailure {
    // 410 keeps showing the library's text, as it did before the failure
    // was typed; only 403 and 404 were reported as gone.
    match http_status(error) {
        Some(403 | 404) => DownloadFailure::Expired,
        _ => DownloadFailure::Other(error.to_string()),
    }
}

/// Whether a group-metadata lookup failed for good: the server answered
/// not-authorized (401), forbidden (403) or item-not-found (404).
pub(super) fn group_lookup_is_final(error: &(impl ErrorChainExt + ?Sized)) -> bool {
    error.server_rejection().is_some_and(|rejection| {
        matches!(rejection.code, 401 | 403 | 404)
            || matches!(
                rejection.text,
                "not-authorized" | "forbidden" | "item-not-found"
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use whatsapp_rust::features::GroupError;
    use whatsapp_rust::http::HttpStatusError;
    use whatsapp_rust::wacore_binary::OwnedNodeRef;
    use whatsapp_rust::wacore_binary::builder::NodeBuilder;
    use whatsapp_rust::{IqError, RejectionStanza};

    fn refused(status: u16) -> anyhow::Error {
        anyhow::Error::new(HttpStatusError { status })
            .context(format!("Download failed with status: {status}"))
    }

    fn rejected(code: u16, text: &str) -> GroupError {
        let node = NodeBuilder::new("iq")
            .attr("type", "error")
            .children([NodeBuilder::new("error")
                .attr("code", code.to_string())
                .attr("text", text.to_owned())
                .build()])
            .build();
        let mut bytes = whatsapp_rust::wacore_binary::marshal::marshal(&node).unwrap();
        bytes.remove(0);
        let response: RejectionStanza = Arc::new(OwnedNodeRef::new(bytes).unwrap()).into();
        GroupError::Iq(IqError::ServerError {
            code,
            text: text.to_owned(),
            error_type: Some("cancel".to_owned()),
            backoff: None,
            response,
        })
    }

    #[test]
    fn gone_media_statuses_ask_for_a_reupload() {
        for status in [403, 404, 410] {
            assert!(needs_reupload(&refused(status)), "{status}");
        }
        for status in [401, 429, 500, 503] {
            assert!(!needs_reupload(&refused(status)), "{status}");
        }
    }

    #[test]
    fn status_numbers_in_unrelated_text_do_not_ask_for_a_reupload() {
        for text in [
            "Download failed with status: 404",
            "connection to 157.240.4.403 timed out",
            "media decryption failed after 410 attempts",
        ] {
            let error = anyhow::anyhow!("{text}");
            assert!(!needs_reupload(&error), "{text}");
            assert_eq!(
                download_failure(&error),
                DownloadFailure::Other(text.into())
            );
        }
    }

    #[test]
    fn missing_media_is_reported_as_expired() {
        assert_eq!(download_failure(&refused(403)), DownloadFailure::Expired);
        assert_eq!(download_failure(&refused(404)), DownloadFailure::Expired);
        assert_eq!(
            download_failure(&refused(410)),
            DownloadFailure::Other("Download failed with status: 410".into())
        );
        assert_eq!(
            download_failure(&refused(500)),
            DownloadFailure::Other("Download failed with status: 500".into())
        );
    }

    #[test]
    fn missing_forbidden_and_unauthorized_groups_are_final() {
        for (code, text) in [
            (404, "item-not-found"),
            (403, "forbidden"),
            (401, "not-authorized"),
        ] {
            assert!(group_lookup_is_final(&rejected(code, text)), "{code}");
            assert!(group_lookup_is_final(&rejected(code, "")), "{code}");
        }
        assert!(group_lookup_is_final(&rejected(400, "forbidden")));
    }

    #[test]
    fn other_group_failures_are_retried() {
        assert!(!group_lookup_is_final(&rejected(429, "rate-overlimit")));
        assert!(!group_lookup_is_final(&rejected(
            500,
            "internal-server-error"
        )));
        assert!(!group_lookup_is_final(&GroupError::Iq(IqError::Timeout)));
        assert!(!group_lookup_is_final(&GroupError::InvalidRequest(
            "forbidden characters in item-not-found".into()
        )));
        assert!(!group_lookup_is_final(&GroupError::Internal(
            anyhow::anyhow!("not-authorized to resolve the lid")
        )));
    }
}
