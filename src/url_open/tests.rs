use super::*;

const URL: &str = "http://127.0.0.1:4000/token/";

fn words(items: &[&str]) -> Vec<String> {
    items.iter().map(|word| (*word).to_owned()).collect()
}

#[test]
fn a_configured_command_replaces_the_platform_openers() {
    let openers = openers(URL, &words(&["my-browser", "--app={url}"]));
    assert_eq!(
        openers,
        vec![UrlOpener::command(
            "my-browser",
            vec![format!("--app={URL}")]
        )]
    );
    assert_eq!(super::openers(URL, &[]), crate::platform::url_openers(URL));
}

#[test]
fn the_first_opener_that_works_ends_the_search() {
    let openers = vec![
        UrlOpener::command("one", Vec::new()),
        UrlOpener::command("two", Vec::new()),
        UrlOpener::command("three", Vec::new()),
    ];
    let mut tried = Vec::new();
    let opened = open_with(&openers, |opener| {
        tried.push(opener.name().to_owned());
        if opener.name() == "two" {
            Attempt::Opened(None)
        } else {
            Attempt::Failed(format!("{} failed", opener.name()))
        }
    });
    assert!(opened.is_ok());
    assert_eq!(tried, ["one", "two"]);
}

#[test]
fn when_every_opener_fails_the_error_names_each_and_no_url() {
    let openers = vec![
        UrlOpener::command("wslview", vec![URL.into()]),
        UrlOpener::command("xdg-open", vec![URL.into()]),
    ];
    let error = open_with(&openers, |opener| {
        Attempt::Failed(format!("{} is missing", opener.name()))
    })
    .unwrap_err();
    assert_eq!(error, "wslview is missing; xdg-open is missing");
    assert_eq!(
        open_with(&[], |_| Attempt::Opened(None)).unwrap_err(),
        "no way to open a browser on this platform"
    );
}

#[cfg(unix)]
mod processes {
    use std::time::Duration;

    use super::*;

    fn sh(script: &str, exit_status_reliable: bool) -> UrlOpener {
        UrlOpener::Command {
            program: "sh".into(),
            args: vec!["-c".into(), script.into()],
            exit_status_reliable,
        }
    }

    #[test]
    fn a_missing_program_and_a_failing_exit_are_failures() {
        let missing = UrlOpener::command("herdr-no-such-opener", vec![URL.into()]);
        let Attempt::Failed(reason) = run(&missing, Duration::from_secs(5)) else {
            panic!("a missing program opened nothing");
        };
        assert!(reason.starts_with("herdr-no-such-opener: "), "{reason}");
        let Attempt::Failed(reason) = run(&sh("exit 3", true), Duration::from_secs(5)) else {
            panic!("exit 3 is a failure");
        };
        assert!(reason.starts_with("sh exited with"), "{reason}");
        assert!(!reason.contains(URL));
    }

    #[test]
    fn a_clean_exit_opens_and_an_unreliable_exit_status_is_not_trusted() {
        assert!(matches!(
            run(&sh("exit 0", true), Duration::from_secs(5)),
            Attempt::Opened(None)
        ));
        assert!(matches!(
            run(&sh("exit 1", false), Duration::from_secs(5)),
            Attempt::Opened(None)
        ));
    }

    #[test]
    fn an_opener_still_running_counts_as_opened_and_is_handed_back() {
        let attempt = run(&sh("sleep 5", true), Duration::from_millis(200));
        let Attempt::Opened(Some(mut child)) = attempt else {
            panic!("a running opener should count as opened");
        };
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
