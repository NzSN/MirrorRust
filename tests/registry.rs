use mirrorrust::{connect_mirror_from_registry, discover_mirrors, Error, TlsOptions};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

fn registry_stub(status: &str, body: &str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let body = body.to_string();
    let status = status.to_string();
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let read = socket.read(&mut request).unwrap();
        let request = String::from_utf8(request[..read].to_vec()).unwrap();
        write!(
            socket,
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        request
    });
    (format!("http://127.0.0.1:{port}/consul"), handle)
}

#[test]
fn registry_discovery_parses_valid_entries_and_skips_invalid_ones() {
    let pin = "ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789";
    let body = format!(
        r#"[
          {{"Service":{{"ID":"first","Address":" 127.0.0.1 ","Port":8823,"Meta":{{"cert-sha256":"{pin}"}}}}}},
          {{"Service":{{"ID":"bad-port","Address":"127.0.0.1","Port":70000}}}},
          {{"Service":{{"ID":"second","Address":"mirror.local","Port":443}}}}
        ]"#
    );
    let (url, server) = registry_stub("200 OK", &body);

    let entries = discover_mirrors(&url);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].id, "first");
    assert_eq!(entries[0].host, "127.0.0.1");
    assert_eq!(entries[0].port, 8823);
    let lower_pin = pin.to_lowercase();
    assert_eq!(entries[0].cert_sha256.as_deref(), Some(lower_pin.as_str()));
    assert_eq!(entries[1].id, "second");
    assert_eq!(entries[1].cert_sha256, None);
    let request = server.join().unwrap();
    assert!(request.starts_with("GET /consul/v1/health/service/modelmirrors HTTP/1.1\r\n"));
    assert!(request.contains("Accept: application/json\r\n"));
}

#[test]
fn registry_discovery_fails_closed_on_http_and_json_errors() {
    let (url, server) = registry_stub("503 Service Unavailable", "[]");
    assert!(discover_mirrors(&url).is_empty());
    server.join().unwrap();

    let (url, server) = registry_stub("200 OK", "not-json");
    assert!(discover_mirrors(&url).is_empty());
    server.join().unwrap();

    assert!(discover_mirrors("ftp://not-supported").is_empty());
}

#[test]
fn registry_connection_fails_closed_when_no_candidates_exist() {
    let (url, server) = registry_stub("200 OK", "[]");
    let options = TlsOptions::new("missing-ca", "missing-cert", "missing-key");

    let error = match connect_mirror_from_registry(&url, &options, None) {
        Ok(_) => panic!("empty registry unexpectedly produced a transport"),
        Err(error) => error,
    };

    assert!(matches!(error, Error::Registry(_)));
    server.join().unwrap();
}
