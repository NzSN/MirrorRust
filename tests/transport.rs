use mirrorrust::{connect_mirror, spawn_mirror, validate_protocol_line, MAX_PROTOCOL_LINE_BYTES};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

#[test]
fn protocol_line_validation_uses_utf8_bytes_and_exact_boundary() {
    assert_eq!(MAX_PROTOCOL_LINE_BYTES, 65_535);
    assert!(validate_protocol_line(&"x".repeat(65_535)).is_ok());
    assert!(validate_protocol_line(&"x".repeat(65_536)).is_err());
    assert!(validate_protocol_line(&("é".repeat(32_767) + "a")).is_ok());
    assert!(validate_protocol_line(&"é".repeat(32_768)).is_err());
    assert!(validate_protocol_line("").is_err());
    assert!(validate_protocol_line("bad\nline").is_err());
}

#[test]
fn rejected_lines_write_nothing_to_stdio_transport() {
    let mut transport = spawn_mirror("/bin/cat").unwrap();
    assert!(transport.send("").is_err());
    assert!(transport.send("bad\nline").is_err());
    assert!(transport.send(&"x".repeat(65_536)).is_err());
    transport.send("good").unwrap();
    assert_eq!(transport.recv().unwrap().as_deref(), Some("good"));
    assert_eq!(transport.close().unwrap(), 0);
}

#[test]
fn tcp_transport_round_trips_one_framed_session() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(socket.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(line, "request\n");
        socket.write_all(b"response\n").unwrap();
    });

    let mut transport = connect_mirror("127.0.0.1", port).unwrap();
    assert!(transport.is_async_capable());
    transport.send("request").unwrap();
    assert_eq!(transport.recv().unwrap().as_deref(), Some("response"));
    transport.close().unwrap();
    peer.join().unwrap();
}
