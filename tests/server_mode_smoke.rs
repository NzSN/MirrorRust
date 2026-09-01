use mirrorrust::{
    as_int, connect_mirror, connect_tls_mirror, get_param, run_client_with_transport,
    spec_from_file, ApalacheConfig, State, StateComputer, TlsOptions, TraceGenerationConfig, Value,
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
    drop(tcp_server);

    let pki = TestPki::generate();
    assert_cn_only_rejected(&pki);
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
    pinned.pin = Some(fingerprint);
    let transport = connect_retry(port, &pinned);
    assert!(transport.is_async_capable());
    run_client_with_transport(
        transport,
        counter_config(),
        trace_config(),
        CounterComputer(BigInt::from(0)),
        Some(inline_spec),
    )
    .expect("Counter replay over mTLS server mode");
}
