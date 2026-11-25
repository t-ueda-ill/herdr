//! `herdr log`: reads pane recordings made with `herdr --log`. Runs locally;
//! no server is needed.

use std::path::PathBuf;

const USAGE: &str = "usage: herdr log show <file> [--time-format <format>]";

#[derive(Debug, PartialEq, Eq)]
struct ShowArgs {
    file: PathBuf,
    time_format: Option<String>,
}

pub(super) fn run_log_command(args: &[String]) -> std::io::Result<i32> {
    let show = match parse_log_args(args) {
        Ok(show) => show,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            eprintln!("{USAGE}");
            return Ok(2);
        }
    };
    match crate::recording::show_file(&show.file, show.time_format.as_deref()) {
        Ok(()) => Ok(0),
        Err(err) => {
            eprintln!("herdr log show: {}: {err}", show.file.display());
            Ok(1)
        }
    }
}

fn parse_log_args(args: &[String]) -> Result<ShowArgs, String> {
    let Some((subcommand, rest)) = args.split_first() else {
        return Err(String::new());
    };
    if subcommand != "show" {
        return Err(format!("unknown log command: {subcommand}"));
    }
    let mut file = None;
    let mut time_format = None;
    let mut rest = rest.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--time-format" => {
                let value = rest.next().ok_or("--time-format needs a value")?;
                time_format = Some(value.clone());
            }
            other if other.starts_with("--time-format=") => {
                time_format = Some(other["--time-format=".len()..].to_owned());
            }
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option: {other}"));
            }
            other if file.is_none() => file = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument: {other}")),
        }
    }
    let file = file.ok_or("missing recording file")?;
    Ok(ShowArgs { file, time_format })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| (*arg).to_owned()).collect()
    }

    #[test]
    fn show_takes_a_file_and_an_optional_time_format() {
        assert_eq!(
            parse_log_args(&args(&["show", "a.cast.gz", "--time-format", "hh:mi"])),
            Ok(ShowArgs {
                file: "a.cast.gz".into(),
                time_format: Some("hh:mi".into()),
            })
        );
        assert_eq!(
            parse_log_args(&args(&["show", "--time-format=ss", "a.cast.gz"])),
            Ok(ShowArgs {
                file: "a.cast.gz".into(),
                time_format: Some("ss".into()),
            })
        );
    }

    #[test]
    fn show_without_a_file_or_with_two_is_rejected() {
        assert!(parse_log_args(&args(&["show"])).is_err());
        assert!(parse_log_args(&args(&["show", "a", "b"])).is_err());
    }

    #[test]
    fn an_unknown_subcommand_or_option_is_rejected() {
        assert!(parse_log_args(&args(&["play", "a"])).is_err());
        assert!(parse_log_args(&args(&["show", "a", "--utc"])).is_err());
    }
}
