//! Reading a `pane.upload.*` response, shared by the CLI and the TUI.

use crate::api::schema::{ResponseResult, SuccessResponse};

fn error_of(response: &serde_json::Value) -> Option<(String, String)> {
    let error = response.get("error")?;
    let text = |key: &str| {
        error
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    Some((text("code"), text("message")))
}

/// The result, or the error's code and message.
pub(crate) fn result_of(response: serde_json::Value) -> Result<ResponseResult, (String, String)> {
    if let Some(error) = error_of(&response) {
        return Err(error);
    }
    serde_json::from_value::<SuccessResponse>(response)
        .map(|success| success.result)
        .map_err(|error| ("invalid_response".into(), error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_and_results_are_told_apart() {
        let error = serde_json::json!({
            "id": "x",
            "error": {"code": "upload_failed", "message": "disk full"}
        });
        assert_eq!(
            result_of(error),
            Err(("upload_failed".into(), "disk full".into()))
        );
        let ok = serde_json::json!({"id": "x", "result": {"type": "ok"}});
        assert!(matches!(result_of(ok), Ok(ResponseResult::Ok {})));
        let garbage = serde_json::json!({"id": "x"});
        assert_eq!(result_of(garbage).unwrap_err().0, "invalid_response");
    }
}
