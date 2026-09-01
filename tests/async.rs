use mirrorrust::{
    await_job, cancel_job, connect_mirror, query_job, spawn_mirror, submit_trace_gen_async,
    submit_validate_async, ApalacheConfig, Error, JobKind, JobOutcome, JobPhase, JobReply,
    SpecResult, TraceGenerationConfig,
};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::thread;

fn cfg() -> ApalacheConfig {
    ApalacheConfig {
        spec_path: "Counter.tla".into(),
        init_predicate: None,
        next_predicate: None,
        const_init: Some("CInit".into()),
        invariant: "TraceComplete".into(),
        length_bound: 6,
        param_vars: Some("parameters".into()),
    }
}

fn scripted_server(
    script: impl FnOnce(Value, &mut std::net::TcpStream) + Send + 'static,
) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(socket.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        script(serde_json::from_str(line.trim_end()).unwrap(), &mut socket);
    });
    (port, handle)
}

fn sequence_server(
    count: usize,
    mut script: impl FnMut(usize, Value, &mut std::net::TcpStream) + Send + 'static,
) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        for index in 0..count {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(!line.is_empty(), "client closed before request {index}");
            script(
                index,
                serde_json::from_str(line.trim_end()).unwrap(),
                &mut socket,
            );
        }
    });
    (port, handle)
}

fn no_bytes_server() -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut bytes = Vec::new();
        socket.read_to_end(&mut bytes).unwrap();
        assert!(
            bytes.is_empty(),
            "preflight wrote protocol bytes: {bytes:?}"
        );
    });
    (port, handle)
}

#[test]
fn submit_validate_async_returns_the_accepted_job() {
    let (port, server) = scripted_server(|request, socket| {
        assert_eq!(request["proto_step"], "register_validate_async");
        assert_eq!(request["bound"], 5);
        writeln!(
            socket,
            r#"{{"proto_step":"job_accepted","jobId":"job-1","kind":"validate"}}"#
        )
        .unwrap();
    });
    let mut transport = connect_mirror("127.0.0.1", port).unwrap();

    let accepted = submit_validate_async(&mut transport, cfg(), 5, None).unwrap();

    assert_eq!(accepted.job_id, "job-1");
    assert_eq!(accepted.kind, JobKind::Validate);
    transport.close().unwrap();
    server.join().unwrap();
}

#[test]
fn query_and_await_preserve_timeout_unknown_and_idempotent_terminal_results() {
    let (port, server) = sequence_server(4, |index, request, socket| match index {
        0 => {
            assert_eq!(request["proto_step"], "query_job");
            writeln!(
                socket,
                r#"{{"proto_step":"job_status","jobId":"job-1","phase":"unknown"}}"#
            )
            .unwrap();
        }
        1 => {
            assert_eq!(request["proto_step"], "await_job");
            assert_eq!(request["timeoutSecs"], 1);
            writeln!(
                socket,
                r#"{{"proto_step":"job_status","jobId":"job-1","phase":"running"}}"#
            )
            .unwrap();
        }
        2 | 3 => {
            assert_eq!(request["proto_step"], "await_job");
            assert!(request.get("timeoutSecs").is_none());
            writeln!(
                socket,
                r#"{{"proto_step":"job_result","jobId":"job-1","outcome":{{"validate":"valid"}}}}"#
            )
            .unwrap();
        }
        _ => unreachable!(),
    });
    let mut transport = connect_mirror("127.0.0.1", port).unwrap();

    assert_eq!(
        query_job(&mut transport, "job-1").unwrap(),
        JobReply::Status {
            job_id: "job-1".into(),
            phase: JobPhase::Unknown,
        }
    );
    assert_eq!(
        await_job(&mut transport, "job-1", Some(1)).unwrap(),
        JobReply::Status {
            job_id: "job-1".into(),
            phase: JobPhase::Running,
        }
    );
    let expected = JobReply::Result {
        job_id: "job-1".into(),
        outcome: JobOutcome::Validate(SpecResult::Valid),
    };
    assert_eq!(await_job(&mut transport, "job-1", None).unwrap(), expected);
    assert_eq!(await_job(&mut transport, "job-1", None).unwrap(), expected);

    transport.close().unwrap();
    server.join().unwrap();
}

#[test]
fn trace_generation_submission_and_cancellation_use_the_job_protocol() {
    let (port, server) = sequence_server(2, |index, request, socket| match index {
        0 => {
            assert_eq!(request["proto_step"], "register_trace_gen_async");
            assert_eq!(request["traceConfig"]["numTraces"], 2);
            writeln!(
                socket,
                r#"{{"proto_step":"job_accepted","jobId":"job-2","kind":"gen_traces"}}"#
            )
            .unwrap();
        }
        1 => {
            assert_eq!(request["proto_step"], "cancel_job");
            assert_eq!(request["jobId"], "job-2");
            writeln!(
                socket,
                r#"{{"proto_step":"job_status","jobId":"job-2","phase":"cancelled"}}"#
            )
            .unwrap();
        }
        _ => unreachable!(),
    });
    let mut transport = connect_mirror("127.0.0.1", port).unwrap();

    let accepted = submit_trace_gen_async(
        &mut transport,
        cfg(),
        TraceGenerationConfig {
            num_traces: 2,
            view: None,
        },
        None,
        None,
    )
    .unwrap();
    assert_eq!(accepted.kind, JobKind::GenTraces);
    assert_eq!(
        cancel_job(&mut transport, &accepted.job_id).unwrap(),
        JobReply::Status {
            job_id: "job-2".into(),
            phase: JobPhase::Cancelled,
        }
    );

    transport.close().unwrap();
    server.join().unwrap();
}

#[test]
fn async_preflight_rejects_stdio_and_invalid_bounds_before_writing() {
    let mut stdio = spawn_mirror("/bin/cat").unwrap();
    let error = submit_validate_async(&mut stdio, cfg(), 5, None).unwrap_err();
    assert!(matches!(error, Error::InvalidArgument(_)));
    stdio.close().unwrap();

    let (port, server) = no_bytes_server();
    let mut network = connect_mirror("127.0.0.1", port).unwrap();
    for bound in [0, 101] {
        let error = submit_validate_async(&mut network, cfg(), bound, None).unwrap_err();
        assert!(matches!(error, Error::InvalidArgument(_)));
    }
    network.close().unwrap();
    server.join().unwrap();
}

#[test]
fn protocol_error_poisons_the_async_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(first.trim_end()).unwrap()["proto_step"],
            "register_validate_async"
        );
        writeln!(
            socket,
            r#"{{"proto_step":"protocol_error","error":"out of phase"}}"#
        )
        .unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        let mut extra = Vec::new();
        let _ = reader.read_to_end(&mut extra);
        assert!(
            extra.is_empty(),
            "poisoned connection sent more bytes: {extra:?}"
        );
    });
    let mut transport = connect_mirror("127.0.0.1", port).unwrap();

    assert!(matches!(
        submit_validate_async(&mut transport, cfg(), 5, None),
        Err(Error::ProtocolError(_))
    ));
    assert!(matches!(
        query_job(&mut transport, "job-1"),
        Err(Error::TransportClosed)
    ));

    server.join().unwrap();
}

#[allow(dead_code)]
fn _trace_config() -> TraceGenerationConfig {
    TraceGenerationConfig {
        num_traces: 2,
        view: None,
    }
}
