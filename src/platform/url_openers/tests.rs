use super::*;

const URL: &str = "http://127.0.0.1:4000/token/a&b.html";

fn args(opener: &UrlOpener) -> Vec<String> {
    match opener {
        UrlOpener::Command { args, .. } => args.clone(),
        #[cfg(windows)]
        UrlOpener::Native { .. } => Vec::new(),
    }
}

#[test]
fn browser_lists_commands_split_at_colons_with_the_url_last_or_at_percent_s() {
    let openers = browser_env_openers("wslview: firefox --new-tab %s ::", URL);
    let names: Vec<_> = openers.iter().map(UrlOpener::name).collect();
    assert_eq!(names, ["wslview", "firefox"]);
    assert_eq!(args(&openers[0]), [URL]);
    assert_eq!(args(&openers[1]), ["--new-tab", URL]);
}

#[test]
fn a_configured_command_takes_the_url_as_one_argument_wherever_it_names_it() {
    let words = |items: &[&str]| items.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    let opener = command_with_url(&words(&["cmd", "--url={url}", "-x"]), "{url}", URL).unwrap();
    assert_eq!(args(&opener), [format!("--url={URL}"), "-x".into()]);
    let appended = command_with_url(&words(&["open", "-a", "Safari"]), "{url}", URL).unwrap();
    assert_eq!(args(&appended), ["-a", "Safari", URL]);
    assert!(command_with_url(&[], "{url}", URL).is_none());
}

#[test]
fn only_an_identical_attempt_is_dropped() {
    let openers = dedup(vec![
        UrlOpener::command("open", vec!["-a".into(), "Missing".into(), URL.into()]),
        UrlOpener::command("wslview", vec![URL.into()]),
        UrlOpener::command("open", vec![URL.into()]),
        UrlOpener::command("wslview", vec![URL.into()]),
    ]);
    assert_eq!(
        openers,
        vec![
            UrlOpener::command("open", vec!["-a".into(), "Missing".into(), URL.into()]),
            UrlOpener::command("wslview", vec![URL.into()]),
            UrlOpener::command("open", vec![URL.into()]),
        ]
    );
}

#[test]
fn a_name_never_shows_the_url() {
    let opener = UrlOpener::command("xdg-open", vec![URL.into()]);
    assert_eq!(opener.name(), "xdg-open");
}
