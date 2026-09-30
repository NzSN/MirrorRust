//! Pre-negotiation consumers must still compile with an exhaustive Error match.
use mirrorrust::{preset_client, run_client, ApalacheConfig, Error, State, TraceGenerationConfig};

fn error_kind(error: &Error) -> &'static str {
    match error {
        Error::SpecInvalid(_) => "spec",
        Error::ProtocolError(_) => "protocol",
        Error::RegisterFailed(_) => "registration",
        Error::StepMismatch { .. } => "mismatch",
        Error::UnexpectedMessage(_) => "unexpected",
        Error::InvalidArgument(_) => "argument",
        Error::SpecSource(_) => "source",
        Error::TransportClosed => "closed",
        Error::Tls(_) => "tls",
        Error::Registry(_) => "registry",
        Error::Io(_) => "io",
        Error::Json(_) => "json",
    }
}

#[cfg(unix)]
#[test]
fn legacy_runner_retains_signature_and_wire_without_negotiation() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    let registration = directory.path().join("registration.json");
    let report = directory.path().join("report.json");
    let fixture = directory.path().join("mirror");
    // The quoted paths come from tempfile, not model or network input.
    let script = format!(
        "#!/bin/sh\nIFS= read -r message || exit 1\nprintf '%s' \"$message\" > '{}'\nprintf '%s\\n' '{{\"proto_step\":\"spec_validated\",\"result\":\"valid\"}}' '{{\"proto_step\":\"initial_state\",\"action\":\"init\",\"state\":{{}}}}'\nIFS= read -r message || exit 1\nprintf '%s' \"$message\" > '{}'\nprintf '%s\\n' '{{\"proto_step\":\"step_ok\"}}' '{{\"proto_step\":\"all_steps_done\"}}'\n",
        registration.display(), report.display()
    );
    std::fs::write(&fixture, script).unwrap();
    std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = ApalacheConfig {
        spec_path: "Counter.tla".into(),
        init_predicate: None,
        next_predicate: None,
        const_init: None,
        invariant: "".into(),
        length_bound: 10,
        param_vars: None,
    };
    let result: Result<(), Error> = run_client(
        fixture.to_str().unwrap(),
        config,
        TraceGenerationConfig {
            num_traces: 1,
            view: None,
        },
        preset_client(vec![State::new()]),
    );
    result.unwrap();
    let registration: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(registration).unwrap()).unwrap();
    assert_eq!(registration["proto_step"], "register");
    assert!(registration.get("modelInterface").is_none());
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(report).unwrap()).unwrap();
    assert_eq!(
        report,
        serde_json::json!({"proto_step": "report_state", "state": {}})
    );
    assert_eq!(error_kind(&Error::TransportClosed), "closed");
}
