//! Read-only requests to any connected server, not only the active one.
//!
//! The Go to picker reads tasks and screens on every server it lists. Why a
//! table apart from `pending_requests`: those answers belong to the active
//! server's projection, and a failed read here must stay silent.

use super::*;
use crate::api::schema::{Method, ResponseResult};

/// What a query answer updates. All belong to the Go to picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum EndpointQueryKind {
    /// `task.list` of one server.
    TaskList,
    /// The preview of one pane.
    Preview(super::goto_preview::GotoPreviewRead),
    /// The full view of one pane.
    FullView(super::goto_preview::GotoPreviewRead),
}

#[derive(Debug)]
pub(crate) struct PendingEndpointQuery {
    pub(super) endpoint_id: ClientEndpointId,
    pub(super) boot_id: String,
    pub(super) kind: EndpointQueryKind,
}

impl ClientShellState {
    /// Whether a query can go to `endpoint_id` now. Why one at a time: the
    /// endpoint lane sends one request at a time, and user actions on the
    /// active server wait behind it.
    pub(super) fn endpoint_query_ready(&self, endpoint_id: &ClientEndpointId) -> bool {
        self.endpoint_is_online(endpoint_id)
            && !self
                .endpoint_queries
                .values()
                .any(|query| &query.endpoint_id == endpoint_id)
    }

    /// Whether `endpoint_id` advertises `method`. Why not the active-server
    /// check: an unknown list counts as supported there, and a server that
    /// cannot answer here must only lose its preview.
    pub(super) fn endpoint_advertises(&self, endpoint_id: &ClientEndpointId, method: &str) -> bool {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.endpoint_id == endpoint_id)
            .and_then(|endpoint| endpoint.methods.as_ref())
            .is_some_and(|methods| methods.contains(method))
    }

    /// Sends `method` to `endpoint_id`. False when the server is offline,
    /// does not advertise the method, or has a query in flight.
    pub(super) fn query_endpoint(
        &mut self,
        endpoint_id: &ClientEndpointId,
        method: Method,
        kind: EndpointQueryKind,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if !self.endpoint_query_ready(endpoint_id)
            || !self.endpoint_advertises(endpoint_id, crate::api::api_method_name(&method))
        {
            return false;
        }
        let Some(boot_id) = self.endpoint_boot_id(endpoint_id).map(str::to_owned) else {
            return false;
        };
        let request_id = format!(
            "{}{}",
            crate::client::endpoint_commands::QUERY_ID_PREFIX,
            self.next_request_id
        );
        self.next_request_id = self.next_request_id.saturating_add(1);
        self.endpoint_queries.insert(
            request_id.clone(),
            PendingEndpointQuery {
                endpoint_id: endpoint_id.clone(),
                boot_id: boot_id.clone(),
                kind,
            },
        );
        outcome.actions.push(ClientShellAction::EndpointQuery {
            endpoint_id: endpoint_id.clone(),
            boot_id,
            request: Box::new(crate::api::schema::Request {
                id: request_id,
                method,
            }),
        });
        true
    }

    pub(crate) fn is_endpoint_query(&self, request_id: &str) -> bool {
        self.endpoint_queries.contains_key(request_id)
    }

    /// Applies a query answer. Returns whether the screen changed.
    pub(crate) fn handle_endpoint_query_result(
        &mut self,
        endpoint_id: &ClientEndpointId,
        boot_id: &str,
        request_id: &str,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(query) = self.endpoint_queries.remove(request_id) else {
            return false;
        };
        let answered_by = endpoint_id;
        let endpoint_id = query.endpoint_id.clone();
        // Why still applied: the preview must leave its loading state.
        let result = if &endpoint_id != answered_by
            || query.boot_id != boot_id
            || self.endpoint_boot_id(&endpoint_id) != Some(boot_id)
        {
            Err(ClientShellEndpointError {
                code: Some("stale_boot".into()),
                message: "the server restarted".into(),
            })
        } else {
            result
        };
        let endpoint_id = &endpoint_id;
        match query.kind {
            EndpointQueryKind::TaskList => self.receive_goto_tasks(endpoint_id, result),
            EndpointQueryKind::Preview(read) => {
                self.receive_goto_preview(endpoint_id, read, result)
            }
            EndpointQueryKind::FullView(read) => self.receive_goto_result(read, result),
        }
    }

    /// A query that will get no answer, such as on a lost connection.
    pub(super) fn cancel_endpoint_query(&mut self, request_id: &str) -> bool {
        let Some(query) = self.endpoint_queries.get(request_id) else {
            return false;
        };
        let endpoint_id = query.endpoint_id.clone();
        let boot_id = query.boot_id.clone();
        self.handle_endpoint_query_result(
            &endpoint_id,
            &boot_id,
            request_id,
            Err(ClientShellEndpointError {
                code: Some("endpoint_cancelled".into()),
                message: "the server did not answer".into(),
            }),
        )
    }
}
