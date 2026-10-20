//! Whether a drop becomes an upload (spec Q3, Q7, Q13, Q15, Q19). A single
//! small image keeps the existing clipboard-image path; the caller checks that
//! first, so this only decides between an upload and a plain paste.

/// Methods a server must advertise for drops to upload.
pub(crate) const UPLOAD_METHODS: [&str; 4] = [
    "pane.upload.begin",
    "pane.upload.cancel",
    "pane.upload.finish",
    "pane.upload.write",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DropGate {
    /// `ui.upload_on_drop`.
    pub(crate) upload_on_drop: bool,
    /// The receiving shell cannot read this machine's files: a remote client,
    /// a non-local endpoint, or an ssh/container pane (`endpoint_accepts_local_images`).
    pub(crate) bridge_active: bool,
    /// Shell mode: methods and capability advertised. Direct attach: the probe
    /// was answered.
    pub(crate) server_supports: bool,
    /// A workspace pane, popup, or directly attached terminal receives input.
    pub(crate) has_target: bool,
}

impl DropGate {
    pub(crate) fn uploads(self) -> bool {
        self.upload_on_drop && self.bridge_active && self.server_supports && self.has_target
    }
}

/// Shell mode: every upload method plus the control capability. Why both:
/// methods alone could come from a server that answers them only through the
/// command lane, which a long upload must not occupy.
pub(crate) fn negotiation_supports_upload(
    supports_method: impl Fn(&str) -> bool,
    supports_capability: impl Fn(&str) -> bool,
) -> bool {
    UPLOAD_METHODS.iter().all(|method| supports_method(method))
        && supports_capability(crate::protocol::endpoint::PANE_UPLOAD_CONTROL_CAPABILITY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_uploads_only_when_every_condition_holds() {
        let all = DropGate {
            upload_on_drop: true,
            bridge_active: true,
            server_supports: true,
            has_target: true,
        };
        assert!(all.uploads());
        for (label, gate) in [
            (
                "ui.upload_on_drop = false",
                DropGate {
                    upload_on_drop: false,
                    ..all
                },
            ),
            (
                "a local host pane (Q3)",
                DropGate {
                    bridge_active: false,
                    ..all
                },
            ),
            (
                "an older server",
                DropGate {
                    server_supports: false,
                    ..all
                },
            ),
            (
                "no input target",
                DropGate {
                    has_target: false,
                    ..all
                },
            ),
        ] {
            assert!(!gate.uploads(), "{label}");
        }
    }

    #[test]
    fn shell_support_needs_all_four_methods_and_the_control_capability() {
        let methods = |missing: Option<&'static str>| {
            move |method: &str| UPLOAD_METHODS.contains(&method) && Some(method) != missing
        };
        let capability = |capability: &str| {
            capability == crate::protocol::endpoint::PANE_UPLOAD_CONTROL_CAPABILITY
        };

        assert!(negotiation_supports_upload(methods(None), capability));
        assert!(!negotiation_supports_upload(
            methods(Some("pane.upload.write")),
            capability
        ));
        assert!(!negotiation_supports_upload(methods(None), |_: &str| false));
    }
}
