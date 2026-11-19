use super::*;

fn parse(head: &str) -> Result<Request, ParseError> {
    parse_request(head.as_bytes())
}

#[test]
fn the_head_ends_after_the_first_blank_line() {
    assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: a\r\n\r\nbody"), Some(27));
    assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: a\r\n"), None);
}

#[test]
fn a_get_request_yields_the_decoded_path_query_and_lowercase_headers() {
    let request = parse(
        "GET /docs/caf%C3%A9%20menu.md?herdr-raw=1 HTTP/1.1\r\nHost: 127.0.0.1:9\r\nSec-Fetch-Dest: document\r\n\r\n",
    )
    .unwrap();
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.path, "/docs/café menu.md");
    assert_eq!(request.query.as_deref(), Some("herdr-raw=1"));
    assert_eq!(request.header("host"), Some("127.0.0.1:9"));
    assert_eq!(request.header("sec-fetch-dest"), Some("document"));
}

#[test]
fn head_is_accepted_and_other_methods_are_not_allowed() {
    assert_eq!(
        parse("HEAD / HTTP/1.1\r\n\r\n").unwrap().method,
        Method::Head
    );
    assert_eq!(
        parse("POST / HTTP/1.1\r\n\r\n"),
        Err(ParseError::MethodNotAllowed)
    );
}

#[test]
fn malformed_request_lines_and_paths_are_bad_requests() {
    for head in [
        "GET\r\n\r\n",
        "GET / FTP/1.0\r\n\r\n",
        "GET relative HTTP/1.1\r\n\r\n",
        "GET /%zz HTTP/1.1\r\n\r\n",
        "GET /%00 HTTP/1.1\r\n\r\n",
        "GET /%ff HTTP/1.1\r\n\r\n",
        "GET / HTTP/1.1\r\nno-colon\r\n\r\n",
    ] {
        assert_eq!(parse(head), Err(ParseError::BadRequest), "{head:?}");
    }
}

#[test]
fn query_parameters_are_looked_up_by_name() {
    let request = parse("GET /?a=1&herdr-token=abc%2B HTTP/1.1\r\n\r\n").unwrap();
    assert_eq!(request.query_param("herdr-token").as_deref(), Some("abc+"));
    assert_eq!(request.query_param("a").as_deref(), Some("1"));
    assert_eq!(request.query_param("missing"), None);
}

#[test]
fn ranges_resolve_against_the_file_length() {
    assert_eq!(parse_range(None, 10), ByteRange::Full);
    assert_eq!(
        parse_range(Some("bytes=2-4"), 10),
        ByteRange::Partial { start: 2, end: 5 }
    );
    assert_eq!(
        parse_range(Some("bytes=7-"), 10),
        ByteRange::Partial { start: 7, end: 10 }
    );
    assert_eq!(
        parse_range(Some("bytes=-3"), 10),
        ByteRange::Partial { start: 7, end: 10 }
    );
    assert_eq!(
        parse_range(Some("bytes=5-99"), 10),
        ByteRange::Partial { start: 5, end: 10 }
    );
    assert_eq!(parse_range(Some("bytes=10-"), 10), ByteRange::Unsatisfiable);
    assert_eq!(parse_range(Some("bytes=-0"), 10), ByteRange::Unsatisfiable);
}

#[test]
fn ranges_the_server_does_not_support_fall_back_to_the_whole_file() {
    for header in ["bytes=0-1,4-5", "items=0-1", "bytes=4-2", "bytes=x-"] {
        assert_eq!(parse_range(Some(header), 10), ByteRange::Full, "{header}");
    }
}

#[test]
fn a_response_head_carries_status_headers_and_closes_the_connection() {
    let head = String::from_utf8(encode_head(
        206,
        &[
            ("Content-Type", "text/plain".into()),
            ("Content-Length", "3".into()),
        ],
    ))
    .unwrap();
    assert_eq!(
        head,
        "HTTP/1.1 206 Partial Content\r\nContent-Type: text/plain\r\nContent-Length: 3\r\nConnection: close\r\n\r\n"
    );
}

#[test]
fn percent_encoding_keeps_path_separators_and_unreserved_bytes() {
    assert_eq!(
        percent_encode_path("docs/café menu.md"),
        "docs/caf%C3%A9%20menu.md"
    );
    assert_eq!(percent_encode_path("a-b_c.~d/e"), "a-b_c.~d/e");
}
