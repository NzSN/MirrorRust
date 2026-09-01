use mirrorrust::{
    as_int, await_job, cancel_job, connect_mirror, connect_mirror_from_registry,
    connect_tls_mirror, get_param, query_job, run_client_validate_transport,
    run_client_with_transport, spec_from_file, submit_validate_async, ApalacheConfig, JobOutcome,
    JobPhase, JobReply, SpecResult, State, StateComputer, TlsOptions, TraceGenerationConfig, Value,
};
use num_bigint::BigInt;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct TestPki {
    _dir: tempfile::TempDir,
    ca: PathBuf,
    server_cert: PathBuf,
    server_key: PathBuf,
    cn_server_cert: PathBuf,
    cn_server_key: PathBuf,
    client_cert: PathBuf,
    client_key: PathBuf,
    rogue_client_cert: PathBuf,
    rogue_client_key: PathBuf,
}

impl TestPki {
    fn generate() -> Self {
        let dir = tempfile::tempdir().expect("TLS fixture tempdir");
        let path = |name: &str| dir.path().join(name);
        std::fs::write(
            path("server.ext"),
            concat!(
                "subjectAltName=IP:127.0.0.1\n",
                "basicConstraints=CA:FALSE\n",
                "keyUsage=digitalSignature,keyEncipherment\n",
                "extendedKeyUsage=serverAuth\n"
            ),
        )
        .unwrap();
        std::fs::write(
            path("client.ext"),
            concat!(
                "basicConstraints=CA:FALSE\n",
                "keyUsage=digitalSignature,keyEncipherment\n",
                "extendedKeyUsage=clientAuth\n"
            ),
        )
        .unwrap();

        openssl(
            dir.path(),
            &[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "ca.key",
                "-out",
                "ca.crt",
                "-days",
                "2",
                "-subj",
                "/CN=MirrorRust Replay CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
            ],
        );
        openssl(
            dir.path(),
            &[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "rogue-ca.key",
                "-out",
                "rogue-ca.crt",
                "-days",
                "2",
                "-subj",
                "/CN=Rogue CA",
                "-addext",
                "basicConstraints=critical,CA:TRUE",
            ],
        );
        openssl(
            dir.path(),
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "rogue-client.key",
                "-out",
                "rogue-client.csr",
                "-subj",
                "/CN=rogue-client",
            ],
        );
        openssl(
            dir.path(),
            &[
                "x509",
                "-req",
                "-in",
                "rogue-client.csr",
                "-CA",
                "rogue-ca.crt",
                "-CAkey",
                "rogue-ca.key",
                "-CAcreateserial",
                "-out",
                "rogue-client.crt",
                "-days",
                "2",
                "-sha256",
                "-extfile",
                "client.ext",
            ],
        );
        openssl(
            dir.path(),
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "cn-server.key",
                "-out",
                "cn-server.csr",
                "-subj",
                "/CN=localhost",
            ],
        );
        openssl(
            dir.path(),
            &[
                "x509",
                "-req",
                "-in",
                "cn-server.csr",
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                "cn-server.crt",
                "-days",
                "2",
                "-sha256",
            ],
        );
        openssl(
            dir.path(),
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "server.key",
                "-out",
                "server.csr",
                "-subj",
                "/CN=127.0.0.1",
            ],
        );
        openssl(
            dir.path(),
            &[
                "x509",
                "-req",
                "-in",
                "server.csr",
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                "server.crt",
                "-days",
                "2",
                "-sha256",
                "-extfile",
                "server.ext",
            ],
        );
        openssl(
            dir.path(),
            &[
                "req",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-keyout",
                "client.key",
                "-out",
                "client.csr",
                "-subj",
                "/CN=mirrorrust-replay-client",
            ],
        );
        openssl(
            dir.path(),
            &[
                "x509",
                "-req",
                "-in",
                "client.csr",
                "-CA",
                "ca.crt",
                "-CAkey",
                "ca.key",
                "-CAcreateserial",
                "-out",
                "client.crt",
                "-days",
                "2",
                "-sha256",
                "-extfile",
                "client.ext",
            ],
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for key in [
                path("server.key"),
                path("cn-server.key"),
                path("client.key"),
                path("rogue-client.key"),
            ] {
                std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }

        Self {
            ca: path("ca.crt"),
            server_cert: path("server.crt"),
            server_key: path("server.key"),
            cn_server_cert: path("cn-server.crt"),
            cn_server_key: path("cn-server.key"),
            client_cert: path("client.crt"),
            client_key: path("client.key"),
            rogue_client_cert: path("rogue-client.crt"),
            rogue_client_key: path("rogue-client.key"),
            _dir: dir,
        }
    }

    fn client_options(&self) -> TlsOptions {
        TlsOptions::new(&self.ca, &self.client_cert, &self.client_key)
    }
}

fn openssl(cwd: &Path, args: &[&str]) {
    let status = Command::new("openssl")
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run openssl");
    assert!(status.success(), "openssl {args:?} failed");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn registry_once(body: String) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(socket.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        assert_eq!(
            request_line,
            "GET /v1/health/service/modelmirrors HTTP/1.1\r\n"
        );
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            if header == "\r\n" || header.is_empty() {
                break;
            }
        }
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    (format!("http://127.0.0.1:{port}"), handle)
}

fn connect_retry(port: u16, options: &TlsOptions) -> mirrorrust::Transport {
    let mut last = None;
    for _ in 0..100 {
        match connect_tls_mirror("127.0.0.1", port, options) {
            Ok(transport) => return transport,
            Err(error) => {
                last = Some(error);
                sleep(Duration::from_millis(100));
            }
        }
    }
    panic!("mTLS server did not become ready: {:?}", last.unwrap());
}

fn connect_tcp_retry(port: u16) -> mirrorrust::Transport {
    let mut last = None;
    for _ in 0..100 {
        match connect_mirror("127.0.0.1", port) {
            Ok(transport) => return transport,
            Err(error) => {
                last = Some(error);
                sleep(Duration::from_millis(100));
            }
        }
    }
    panic!("TCP server did not become ready: {:?}", last.unwrap());
}

fn assert_cn_only_rejected(pki: &TestPki) {
    let port = free_port();
    let child = Command::new("openssl")
        .args([
            "s_server",
            "-Verify",
            "1",
            "-verify_return_error",
            "-tls1_3",
            "-quiet",
            "-accept",
            &port.to_string(),
            "-cert",
            pki.cn_server_cert.to_str().unwrap(),
            "-key",
            pki.cn_server_key.to_str().unwrap(),
            "-CAfile",
            pki.ca.to_str().unwrap(),
            "-naccept",
            "1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn CN-only TLS peer");
    let _peer = ChildGuard(child);
    let mut options = pki.client_options();
    options.server_name = Some("localhost".into());
    for _ in 0..100 {
        match connect_tls_mirror("127.0.0.1", port, &options) {
            Ok(_) => panic!("CN-only server certificate was accepted"),
            Err(mirrorrust::Error::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionRefused =>
            {
                sleep(Duration::from_millis(50));
            }
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("not valid for name")
                        || message.contains("certificate")
                        || message.contains("CertNotValidForName"),
                    "unexpected CN-only rejection: {message}"
                );
                return;
            }
        }
    }
    panic!("CN-only TLS peer did not become ready");
}

fn assert_tls12_only_rejected(pki: &TestPki) {
    let port = free_port();
    let child = Command::new("openssl")
        .args([
            "s_server",
            "-Verify",
            "1",
            "-verify_return_error",
            "-tls1_2",
            "-quiet",
            "-accept",
            &port.to_string(),
            "-cert",
            pki.server_cert.to_str().unwrap(),
            "-key",
            pki.server_key.to_str().unwrap(),
            "-CAfile",
            pki.ca.to_str().unwrap(),
            "-naccept",
            "1",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn TLS 1.2-only peer");
    let _peer = ChildGuard(child);
    for _ in 0..100 {
        match connect_tls_mirror("127.0.0.1", port, &pki.client_options()) {
            Ok(_) => panic!("TLS 1.2-only server was accepted"),
            Err(mirrorrust::Error::Io(error))
                if error.kind() == std::io::ErrorKind::ConnectionRefused =>
            {
                sleep(Duration::from_millis(50));
            }
            Err(error) => {
                assert!(
                    error.to_string().contains("TLS")
                        || error.to_string().contains("protocol")
                        || error.to_string().contains("alert"),
                    "unexpected TLS 1.2 rejection: {error}"
                );
                return;
            }
        }
    }
    panic!("TLS 1.2-only peer did not become ready");
}

fn st(pairs: Vec<(&str, Value)>) -> State {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

struct CounterComputer(BigInt);

impl StateComputer for CounterComputer {
    fn compute(&mut self, _action: &str, params: &State, prev: &State) -> State {
        if !prev.contains_key("count") {
            self.0 = BigInt::from(0);
        } else {
            let stride = get_param(params, "parameters")
                .and_then(|record| record.get("stride"))
                .and_then(as_int)
                .expect("Counter step has an integer stride");
            self.0 += stride;
        }
        st(vec![("count", Value::Int(self.0.clone()))])
    }
}

struct IncorrectCounterComputer(CounterComputer);

impl StateComputer for IncorrectCounterComputer {
    fn compute(&mut self, action: &str, params: &State, prev: &State) -> State {
        let mut state = self.0.compute(action, params, prev);
        state.insert("unexpected".into(), Value::Bool(true));
        state
    }
}

fn assert_counter_mismatch(
    transport: mirrorrust::Transport,
    inline_spec: mirrorrust::ApalacheSpec,
) {
    let error = run_client_with_transport(
        transport,
        counter_config(),
        trace_config(),
        IncorrectCounterComputer(CounterComputer(BigInt::from(0))),
        Some(inline_spec),
    )
    .expect_err("an extra observable state key must cause step_mismatch");
    match error {
        mirrorrust::Error::StepMismatch { hints, .. } => assert!(
            hints
                .iter()
                .any(|hint| matches!(hint, mirrorrust::DiffHint::Extra { .. })),
            "step_mismatch did not include an extra-key hint: {hints:?}"
        ),
        other => panic!("expected step_mismatch, got {other:?}"),
    }
}

fn counter_config() -> ApalacheConfig {
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

fn trace_config() -> TraceGenerationConfig {
    TraceGenerationConfig {
        num_traces: 10,
        view: Some("View".into()),
    }
}

#[test]
fn counter_replays_over_real_tcp_and_mtls_server_modes() {
    let bin = match std::env::var("MIRROR_BIN") {
        Ok(bin) if !bin.is_empty() => bin,
        _ => {
            eprintln!("MIRROR_BIN not set; skipping server-mode smoke test");
            return;
        }
    };
    let spec_path = std::env::var("SPEC").unwrap_or_else(|_| "specs/Counter.tla".to_string());
    let inline_spec = spec_from_file(&spec_path).expect("Counter inline spec closure");

    let tcp_port = free_port();
    let tcp_child = Command::new(&bin)
        .args(["--serve", &tcp_port.to_string(), "--jobs", "2"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn mirror --serve");
    let tcp_server = ChildGuard(tcp_child);
    let tcp = connect_tcp_retry(tcp_port);
    assert!(tcp.is_async_capable());
    run_client_with_transport(
        tcp,
        counter_config(),
        trace_config(),
        CounterComputer(BigInt::from(0)),
        Some(inline_spec.clone()),
    )
    .expect("Counter replay over TCP server mode");
    assert_counter_mismatch(connect_tcp_retry(tcp_port), inline_spec.clone());
    drop(tcp_server);

    let pki = TestPki::generate();
    assert_cn_only_rejected(&pki);
    assert_tls12_only_rejected(&pki);
    let port = free_port();
    let child = Command::new(&bin)
        .args([
            "--server",
            &port.to_string(),
            "--tls",
            "--cert",
            pki.server_cert.to_str().unwrap(),
            "--key",
            pki.server_key.to_str().unwrap(),
            "--ca",
            pki.ca.to_str().unwrap(),
            "--jobs",
            "2",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn mirror --server");
    let _server = ChildGuard(child);

    // First handshake captures the leaf fingerprint. Reconnect with an
    // uppercase pin to verify case-insensitive C26 matching before replay.
    let probe = connect_retry(port, &pki.client_options());
    let fingerprint = probe
        .peer_fingerprint()
        .expect("peer fingerprint")
        .to_uppercase();
    assert_eq!(fingerprint.len(), 64);
    drop(probe);

    let mut wrong_pin = pki.client_options();
    wrong_pin.pin = Some("0".repeat(64));
    let error = connect_tls_mirror("127.0.0.1", port, &wrong_pin)
        .err()
        .expect("wrong pin must fail");
    assert!(error.to_string().contains("fingerprint mismatch"));

    let rogue = TlsOptions::new(&pki.ca, &pki.rogue_client_cert, &pki.rogue_client_key);
    match connect_tls_mirror("127.0.0.1", port, &rogue) {
        Err(_) => {}
        Ok(transport) => assert!(
            run_client_with_transport(
                transport,
                counter_config(),
                trace_config(),
                CounterComputer(BigInt::from(0)),
                Some(inline_spec.clone()),
            )
            .is_err(),
            "a client certificate signed by a rogue CA completed a protocol session"
        ),
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let loose_key = pki._dir.path().join("client-loose.key");
        std::fs::copy(&pki.client_key, &loose_key).unwrap();
        std::fs::set_permissions(&loose_key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let loose = TlsOptions::new(&pki.ca, &pki.client_cert, loose_key);
        let error = connect_tls_mirror("127.0.0.1", port, &loose)
            .err()
            .expect("group-readable key must fail");
        assert!(error.to_string().contains("chmod 0600"));
    }

    let mut pinned = pki.client_options();
    pinned.pin = Some(fingerprint.clone());
    let transport = connect_retry(port, &pinned);
    assert!(transport.is_async_capable());
    run_client_with_transport(
        transport,
        counter_config(),
        trace_config(),
        CounterComputer(BigInt::from(0)),
        Some(inline_spec.clone()),
    )
    .expect("Counter replay over mTLS server mode");

    let service = |id: &str, advertised_pin: &str| {
        format!(
            r#"{{"Service":{{"ID":"{id}","Address":"127.0.0.1","Port":{port},"Meta":{{"cert-sha256":"{advertised_pin}"}}}}}}"#
        )
    };
    let (registry, registry_server) =
        registry_once(format!("[{}]", service("mirror", &fingerprint)));
    let discovered = connect_mirror_from_registry(&registry, &pki.client_options(), None)
        .expect("registry-discovered pinned mTLS connection");
    registry_server.join().unwrap();
    run_client_with_transport(
        discovered,
        counter_config(),
        trace_config(),
        CounterComputer(BigInt::from(0)),
        Some(inline_spec.clone()),
    )
    .expect("Counter replay through registry-discovered mTLS");

    // Registry candidates are tried in order; a wrong advertised pin must
    // fail before protocol bytes and the next correctly pinned peer succeeds.
    let (registry, registry_server) = registry_once(format!(
        "[{},{}]",
        service("wrong-pin", &"0".repeat(64)),
        service("right-pin", &fingerprint)
    ));
    let mut failover = connect_mirror_from_registry(&registry, &pki.client_options(), None)
        .expect("registry pin failure should fall through to the next candidate");
    registry_server.join().unwrap();
    failover.close().unwrap();

    // An explicit deployment pin overrides stale registry metadata.
    let (registry, registry_server) =
        registry_once(format!("[{}]", service("override", &"0".repeat(64))));
    let mut overridden =
        connect_mirror_from_registry(&registry, &pki.client_options(), Some(&fingerprint))
            .expect("explicit pin override");
    registry_server.join().unwrap();
    overridden.close().unwrap();

    assert_counter_mismatch(connect_retry(port, &pinned), inline_spec);
}

#[test]
fn async_jobs_cover_cross_connection_long_poll_ordering_cancellation_and_eviction() {
    let bin = match std::env::var("MIRROR_BIN") {
        Ok(bin) if !bin.is_empty() => bin,
        _ => {
            eprintln!("MIRROR_BIN not set; skipping async server-mode smoke test");
            return;
        }
    };
    let spec_path = std::env::var("SPEC").unwrap_or_else(|_| "specs/Counter.tla".to_string());
    let inline_spec = spec_from_file(&spec_path).expect("Counter inline spec closure");
    let port = free_port();
    let child = Command::new(&bin)
        .args(["--serve", &port.to_string(), "--jobs", "2"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn async mirror --serve");
    let _server = ChildGuard(child);

    // C20: the async validate payload is identical to the synchronous
    // register_validate verdict for the same config and bound.
    run_client_validate_transport(
        connect_tcp_retry(port),
        counter_config(),
        1,
        Some(inline_spec.clone()),
    )
    .expect("sync Counter validation at bound 1");

    let mut submitter = connect_tcp_retry(port);
    let mut observer = connect_tcp_retry(port);
    let accepted = submit_validate_async(
        &mut submitter,
        counter_config(),
        1,
        Some(inline_spec.clone()),
    )
    .expect("submit validate job");
    let observed = query_job(&mut observer, &accepted.job_id).expect("cross-connection query");
    assert!(matches!(
        observed,
        JobReply::Status {
            phase: JobPhase::Pending | JobPhase::Running | JobPhase::Done,
            ..
        } | JobReply::Result { .. }
    ));
    let terminal =
        await_job(&mut observer, &accepted.job_id, None).expect("cross-connection await");
    assert_eq!(
        terminal,
        JobReply::Result {
            job_id: accepted.job_id.clone(),
            outcome: JobOutcome::Validate(SpecResult::Valid),
        }
    );
    assert_eq!(
        await_job(&mut observer, &accepted.job_id, None).expect("idempotent terminal await"),
        terminal
    );
    submitter.close().unwrap();

    // C23: two jobs may complete in either order; identify results by job id
    // and explicitly await them in reverse submission order.
    let mut ordering = connect_tcp_retry(port);
    let valid = submit_validate_async(
        &mut ordering,
        counter_config(),
        1,
        Some(inline_spec.clone()),
    )
    .expect("submit valid job");
    let invalid = submit_validate_async(
        &mut ordering,
        counter_config(),
        6,
        Some(inline_spec.clone()),
    )
    .expect("submit invalid job");
    assert!(matches!(
        await_job(&mut observer, &invalid.job_id, None).expect("await second job first"),
        JobReply::Result {
            outcome: JobOutcome::Validate(SpecResult::Invalid(_)),
            ..
        }
    ));
    assert_eq!(
        await_job(&mut observer, &valid.job_id, None).expect("await first job second"),
        JobReply::Result {
            job_id: valid.job_id,
            outcome: JobOutcome::Validate(SpecResult::Valid),
        }
    );
    ordering.close().unwrap();

    // C19: use a non-violating invariant at bound 100 so the job remains
    // alive long enough for cooperative cancellation to terminate apalache.
    let source = std::fs::read_to_string(&spec_path).expect("Counter source");
    let slow_source = source.replace(
        "========================================================",
        "NeverNegative == count >= 0\n========================================================",
    );
    let slow_spec = mirrorrust::ApalacheSpec {
        sources: vec![slow_source],
    };
    let mut slow_cfg = counter_config();
    slow_cfg.invariant = "NeverNegative".into();
    let mut owner = connect_tcp_retry(port);
    let slow = submit_validate_async(&mut owner, slow_cfg.clone(), 100, Some(slow_spec.clone()))
        .expect("submit cancellable job");
    let cancelled = cancel_job(&mut owner, &slow.job_id).expect("cancel running job");
    assert!(matches!(
        cancelled,
        JobReply::Status {
            phase: JobPhase::Cancelled,
            ..
        } | JobReply::Result {
            outcome: JobOutcome::InfraError(_),
            ..
        }
    ));

    // C6/C21: disconnecting the submitting connection cancels and evicts
    // its jobs; a different connection observes the exact unknown phase.
    let doomed = submit_validate_async(&mut owner, slow_cfg.clone(), 100, Some(slow_spec.clone()))
        .expect("submit job to be evicted");
    owner.close().unwrap();
    let mut phase = None;
    for _ in 0..100 {
        match query_job(&mut observer, &doomed.job_id).expect("query evicted job") {
            JobReply::Status {
                phase: JobPhase::Unknown,
                ..
            } => {
                phase = Some(JobPhase::Unknown);
                break;
            }
            _ => sleep(Duration::from_millis(100)),
        }
    }
    assert_eq!(phase, Some(JobPhase::Unknown));
    observer.close().unwrap();

    // C22: a one-slot job store rejects the second live submission
    // synchronously with register_error instead of silently queueing it.
    let queue_port = free_port();
    let queue_child = Command::new(&bin)
        .args(["--serve", &queue_port.to_string(), "--jobs", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn one-slot mirror --serve");
    let _queue_server = ChildGuard(queue_child);
    let mut queue_conn = connect_tcp_retry(queue_port);
    let first = submit_validate_async(
        &mut queue_conn,
        slow_cfg.clone(),
        100,
        Some(slow_spec.clone()),
    )
    .expect("fill one-slot job store");
    let full = submit_validate_async(&mut queue_conn, slow_cfg, 100, Some(slow_spec));
    match full {
        Err(mirrorrust::Error::RegisterFailed(message)) => {
            assert!(message.to_ascii_lowercase().contains("queue full"));
        }
        other => panic!("expected queue-full register_error, got {other:?}"),
    }
    let _ = cancel_job(&mut queue_conn, &first.job_id);
    queue_conn.close().unwrap();
}
