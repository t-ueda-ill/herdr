use crate::api::schema::Location;

/// Keeps only the entries of `result.<list_key>` whose `location` matches `filter`.
pub(crate) fn retain_at_location(response: &mut serde_json::Value, list_key: &str, filter: &str) {
    let Some(entries) = response
        .get_mut("result")
        .and_then(|result| result.get_mut(list_key))
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    entries.retain(|entry| {
        // Why not keep entries without a location: an older server sends none,
        // and showing them would claim they match.
        entry
            .get("location")
            .and_then(|location| location.get("id"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| Location::id_matches(id, filter))
    });
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::retain_at_location;

    fn agent(name: &str, location_id: Option<&str>) -> serde_json::Value {
        let mut agent = json!({ "name": name });
        if let Some(id) = location_id {
            agent["location"] = json!({ "id": id });
        }
        agent
    }

    fn names(response: &serde_json::Value) -> Vec<&str> {
        response["result"]["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| agent["name"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn keeps_agents_on_the_host_and_in_its_containers() {
        let mut response = json!({ "result": { "agents": [
            agent("local", Some("local")),
            agent("api", Some("ssh:devbox")),
            agent("etl", Some("ssh:devbox/container:etl")),
            agent("other", Some("ssh:devbox2")),
        ]}});

        retain_at_location(&mut response, "agents", "ssh:devbox");

        assert_eq!(names(&response), ["api", "etl"]);
    }

    #[test]
    fn drops_agents_whose_server_sent_no_location() {
        let mut response = json!({ "result": { "agents": [agent("old", None)] }});

        retain_at_location(&mut response, "agents", "local");

        assert!(names(&response).is_empty());
    }

    #[test]
    fn leaves_an_error_response_untouched() {
        let mut response = json!({ "error": { "code": "x", "message": "y" } });
        let before = response.clone();

        retain_at_location(&mut response, "agents", "local");

        assert_eq!(response, before);
    }
}
