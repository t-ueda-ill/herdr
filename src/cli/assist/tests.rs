use super::*;

fn parse(args: &[&str]) -> Result<Parsed, ParseError> {
    request(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
}

fn params(args: &[&str]) -> serde_json::Value {
    serde_json::to_value(parse(args).unwrap().request).unwrap()["params"].clone()
}

#[test]
fn start_names_the_pane_product_and_placement() {
    let parsed = parse(&["start", "w1-2", "--with", "codex", "--right", "--json"]).unwrap();

    assert_eq!(
        serde_json::to_value(parsed.request).unwrap(),
        serde_json::json!({
            "id": "cli:assist:start",
            "method": "assist.start",
            "params": {
                "pane_id": "w1-2",
                "product": "codex",
                "placement": "right",
                "focus": true,
            }
        })
    );
    assert!(parsed.json);
}

#[test]
fn start_without_a_product_or_placement_leaves_both_to_the_server() {
    assert_eq!(
        params(&["start", "w1-2", "--no-focus"]),
        serde_json::json!({"pane_id": "w1-2", "placement": "auto", "focus": false})
    );
    assert_eq!(params(&["start", "w1-2", "--below"])["placement"], "below");
}

#[test]
fn right_and_below_cannot_be_given_together() {
    assert!(parse(&["start", "w1-2", "--right", "--below"]).is_err());
}

#[test]
fn an_unknown_product_is_a_usage_error() {
    assert!(parse(&["start", "w1-2", "--with", "gemini"]).is_err());
}

#[test]
fn refresh_list_and_products_name_their_methods() {
    assert_eq!(
        serde_json::to_value(parse(&["refresh", "w1-3"]).unwrap().request).unwrap(),
        serde_json::json!({
            "id": "cli:assist:refresh",
            "method": "assist.refresh",
            "params": {"pane_id": "w1-3"}
        })
    );
    assert_eq!(
        serde_json::to_value(parse(&["list"]).unwrap().request).unwrap(),
        serde_json::json!({"id": "cli:assist:list", "method": "assist.list", "params": {}})
    );
    assert_eq!(
        serde_json::to_value(parse(&["products"]).unwrap().request).unwrap(),
        serde_json::json!({"id": "cli:assist:products", "method": "assist.products", "params": {}})
    );
}

#[test]
fn an_assist_for_a_closed_pane_says_so() {
    let assist = AssistInfo {
        pane_id: "w1-3".into(),
        target_pane_id: None,
        product: AssistProduct::Claude,
        context_path: "/tmp/x/context.md".into(),
    };

    assert_eq!(
        format_assists(&[assist]),
        "w1-3 (claude) for a closed pane\n"
    );
}
