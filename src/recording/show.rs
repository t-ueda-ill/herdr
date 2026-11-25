//! `herdr log show`: prints an asciicast recording with the local date and
//! time in front of every event, so a recording stays playable by asciinema
//! and still reads like a timestamped log.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;

use time::{OffsetDateTime, UtcOffset};

use super::clock::TimeFormat;

/// Prints `path` (gzip-compressed or plain asciicast v2) to stdout.
/// `time_format` overrides `[log] time_format`.
pub(crate) fn show_file(path: &Path, time_format: Option<&str>) -> io::Result<()> {
    let format = match time_format {
        Some(text) => TimeFormat::parse(text)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?,
        None => {
            super::RecordingFormat::from_config(&crate::config::Config::load().config.log)
                .time_format
        }
    };
    let input = open(path)?;
    let stdout = io::stdout();
    let mut out = io::BufWriter::new(stdout.lock());
    match show(input, &mut out, &format, platform_offset_at).and_then(|()| out.flush()) {
        // Why not an error: `herdr log show FILE | head` closes the pipe early.
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

fn open(path: &Path) -> io::Result<Box<dyn BufRead>> {
    let mut file = BufReader::new(File::open(path)?);
    let gzip = file.fill_buf()?.starts_with(&[0x1f, 0x8b]);
    Ok(if gzip {
        Box::new(BufReader::new(flate2::read::MultiGzDecoder::new(file)))
    } else {
        Box::new(file)
    })
}

/// The local offset from UTC at `secs` since the epoch.
fn platform_offset_at(secs: i64) -> Option<i32> {
    let local = crate::platform::local_datetime_at(secs)?;
    i32::try_from(local.assume_utc().unix_timestamp() - secs).ok()
}

/// Writes each event of `input` as `<time> <code> <data as JSON>`. The
/// header line is kept as it is, and so is a line that does not parse, such
/// as the half-written last line of a recording still in progress.
///
/// `offset_at` gives the local offset from UTC at a time, so a recording that
/// spans a daylight-saving change shows each event in the offset of its day.
pub(crate) fn show(
    input: impl Read,
    out: &mut impl Write,
    format: &TimeFormat,
    mut offset_at: impl FnMut(i64) -> Option<i32>,
) -> io::Result<()> {
    let mut origin: Option<i64> = None;
    let mut offset_cache: Option<(i64, UtcOffset)> = None;
    for line in BufReader::new(input).lines() {
        let line = match line {
            Ok(line) => line,
            // A file still being written has no gzip trailer yet.
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err),
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            writeln!(out, "{line}")?;
            continue;
        };
        if let Some(timestamp) = value.get("timestamp").and_then(serde_json::Value::as_i64) {
            origin = Some(timestamp);
            writeln!(out, "{line}")?;
            continue;
        }
        let event = (
            origin,
            value.get(0).and_then(serde_json::Value::as_f64),
            value.get(1).and_then(serde_json::Value::as_str),
            value.get(2),
        );
        let (Some(origin), Some(elapsed), Some(code), Some(data)) = event else {
            writeln!(out, "{line}")?;
            continue;
        };
        // Why skip: every line now starts with the time, so the per-minute
        // time markers would only repeat it. Herdr's other markers start with
        // `herdr:` and stay.
        if code == "m" && !data.as_str().is_some_and(|text| text.starts_with("herdr:")) {
            continue;
        }
        // Why add whole seconds apart: an f64 of epoch nanoseconds has only
        // about 256 ns resolution and rounds 0.250 down to 0.249.
        let nanos =
            i128::from(origin) * 1_000_000_000 + (elapsed * 1_000_000_000.0).round() as i128;
        let Ok(at) = OffsetDateTime::from_unix_timestamp_nanos(nanos) else {
            writeln!(out, "{line}")?;
            continue;
        };
        // Offsets change on whole minutes at most, so one lookup per minute.
        let minute = at.unix_timestamp().div_euclid(60);
        let offset = match offset_cache {
            Some((cached, offset)) if cached == minute => offset,
            _ => {
                let offset = offset_at(at.unix_timestamp())
                    .and_then(|secs| UtcOffset::from_whole_seconds(secs).ok())
                    .unwrap_or(UtcOffset::UTC);
                offset_cache = Some((minute, offset));
                offset
            }
        };
        writeln!(out, "{} {code} {data}", format.spell(at.to_offset(offset)))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDING: &str = concat!(
        r#"{"version":2,"width":80,"height":24,"timestamp":1795000000,"title":"herdr default repo pane 3"}"#,
        "\n",
        r#"[0.250000, "m", "[2026/11/18 20:06:40.250]"]"#,
        "\n",
        r#"[0.250000, "o", "$ ls\r\n\u001b[0m"]"#,
        "\n",
        r#"[61.500000, "r", "100x30"]"#,
        "\n",
    );

    fn shown(input: &[u8], format: &str, offset: Option<i32>) -> String {
        let mut out = Vec::new();
        show(input, &mut out, &TimeFormat::parse(format).unwrap(), |_| {
            offset
        })
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn every_event_gets_the_local_date_and_time_from_the_header_and_its_seconds() {
        let text = shown(
            RECORDING.as_bytes(),
            "[yyyy/mm/dd hh:mi:ss.sss]",
            Some(9 * 3600),
        );
        let lines: Vec<_> = text.lines().collect();
        assert!(lines[0].starts_with(r#"{"version":2"#));
        assert_eq!(
            lines[1],
            r#"[2026/11/18 20:06:40.250] o "$ ls\r\n\u001b[0m""#
        );
        assert_eq!(lines[2], r#"[2026/11/18 20:07:41.500] r "100x30""#);
    }

    #[test]
    fn the_format_decides_how_the_time_is_spelled() {
        let text = shown(RECORDING.as_bytes(), "hh:mi", None);
        assert_eq!(text.lines().nth(1), Some(r#"11:06 o "$ ls\r\n\u001b[0m""#));
    }

    #[test]
    fn a_line_that_does_not_parse_is_kept_as_it_is() {
        let input = format!("{RECORDING}[62.0, \"o\", \"cut");
        let text = shown(input.as_bytes(), "ss", None);
        assert_eq!(text.lines().last(), Some("[62.0, \"o\", \"cut"));
    }

    #[test]
    fn time_markers_are_left_out_and_herdrs_other_markers_kept() {
        let input = format!(
            "{RECORDING}{}\n",
            r#"[62.0, "m", "herdr: 10 bytes not recorded"]"#
        );
        let text = shown(input.as_bytes(), "ss", None);
        assert!(!text.contains("20:06:40"), "{text}");
        assert_eq!(
            text.lines().last(),
            Some(r#"42 m "herdr: 10 bytes not recorded""#)
        );
    }

    #[test]
    fn the_offset_is_looked_up_for_each_events_own_time() {
        let mut asked = Vec::new();
        let mut out = Vec::new();
        show(
            RECORDING.as_bytes(),
            &mut out,
            &TimeFormat::parse("hh").unwrap(),
            |secs| {
                asked.push(secs);
                None
            },
        )
        .unwrap();
        // One lookup per minute: 11:06 for the first two events, 11:07 for the third.
        assert_eq!(asked, vec![1_795_000_000, 1_795_000_061]);
    }

    #[test]
    fn a_gzip_file_still_being_written_is_shown_up_to_what_was_flushed() {
        let dir =
            std::env::temp_dir().join(format!("herdr-recording-show-{}-open", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("open.cast.gz");
        let mut encoder = flate2::write::GzEncoder::new(
            File::create(&path).unwrap(),
            flate2::Compression::fast(),
        );
        encoder.write_all(RECORDING.as_bytes()).unwrap();
        encoder.flush().unwrap();

        let mut out = Vec::new();
        show(
            open(&path).unwrap(),
            &mut out,
            &TimeFormat::parse("ss").unwrap(),
            |_| None,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap().lines().count(), 3);
        drop(encoder);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
