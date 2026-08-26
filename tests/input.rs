//! Spec input resolution: `--spec` accepts a filesystem path or an
//! http(s) URL (e.g. https://openapi.cadenya.com/api-spec.yml).

use std::io::{Read, Write};
use std::net::TcpListener;

/// One-shot HTTP stub on a random local port.
fn serve_once(status_line: &'static str, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let response = format!(
            "HTTP/1.1 {status_line}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).expect("write");
    });
    format!("http://{addr}/api-spec.yml")
}

#[test]
fn url_specs_are_fetched() {
    let url = serve_once("200 OK", "openapi: 3.1.0\n");
    let source = redwood::input::read_spec(&url).expect("fetches");
    assert_eq!(source, "openapi: 3.1.0\n");
}

#[test]
fn http_errors_name_the_status_and_url() {
    let url = serve_once("404 Not Found", "nope");
    let err = redwood::input::read_spec(&url).expect_err("404 must fail");
    let msg = format!("{err:#}");
    assert!(msg.contains("404"), "{msg}");
    assert!(msg.contains("/api-spec.yml"), "{msg}");
}

#[test]
fn file_specs_still_read_from_disk() {
    let source = redwood::input::read_spec(concat!(env!("CARGO_MANIFEST_DIR"), "/api-spec.yml"))
        .expect("reads");
    assert!(source.contains("openapi:"));
}

#[test]
fn missing_files_name_the_path() {
    let err = redwood::input::read_spec("/nonexistent/spec.yml").expect_err("must fail");
    assert!(format!("{err:#}").contains("/nonexistent/spec.yml"));
}
