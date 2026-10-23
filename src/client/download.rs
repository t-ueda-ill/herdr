//! Downloading files from the file workbench to this machine (design §4).
//!
//! Pure decisions live in `destination` and the gate below; transfers run on a
//! worker thread and the client loop only executes the effects they return.

pub(super) mod destination;
pub(super) mod runtime;
pub(super) mod transfer;
pub(super) mod worker;

/// Methods a server must advertise for downloads.
pub(crate) const DOWNLOAD_METHODS: [&str; 3] = [
    "pane.download.begin",
    "pane.download.cancel",
    "pane.download.read",
];

/// Shell mode: every download method plus the control capability. Why both:
/// see `upload::gate::negotiation_supports_transfer`.
pub(crate) fn negotiation_supports_download(
    supports_method: impl Fn(&str) -> bool,
    supports_capability: impl Fn(&str) -> bool,
) -> bool {
    super::upload::gate::negotiation_supports_transfer(
        &DOWNLOAD_METHODS,
        crate::protocol::endpoint::PANE_DOWNLOAD_CONTROL_CAPABILITY,
        supports_method,
        supports_capability,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::endpoint::PANE_DOWNLOAD_CONTROL_CAPABILITY;

    fn methods(missing: Option<&'static str>) -> impl Fn(&str) -> bool {
        move |method| DOWNLOAD_METHODS.contains(&method) && Some(method) != missing
    }

    fn capability(name: &str) -> bool {
        name == PANE_DOWNLOAD_CONTROL_CAPABILITY
    }

    #[test]
    fn downloads_need_all_three_methods_and_the_control_capability() {
        assert!(negotiation_supports_download(methods(None), capability));
        for method in DOWNLOAD_METHODS {
            assert!(
                !negotiation_supports_download(methods(Some(method)), capability),
                "without {method}"
            );
        }
        assert!(!negotiation_supports_download(methods(None), |_| false));
    }

    #[test]
    fn the_upload_capability_does_not_enable_downloads() {
        assert!(!negotiation_supports_download(methods(None), |name| {
            name == crate::protocol::endpoint::PANE_UPLOAD_CONTROL_CAPABILITY
        }));
    }
}
