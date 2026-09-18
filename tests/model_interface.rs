use mirrorrust::{
    make_verify_request, run_client_with_traces_negotiated_transport, spawn_mirror, ApalacheConfig,
    BindingError, CompiledAdapterKey, CompiledAdapterRegistration, CompiledAdapterRegistry,
    CompiledAdapterSelection, Error, GeneratedModelInterface, LocalBinding, NegotiationPolicy,
    SemanticDigest, State, STATE_COMPUTER_CONTRACT_VERSION,
};
use std::cell::RefCell;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;

const DIGEST: &str = "193d6cc187d05c18f02ad483a44f8ad0c1634b02083df241df08b9281b045d1c";
const CONTRACT: &str = r#"{"actions":[{"id":"Tick","inputs":[{"from":{"path":[{"field":"parameters"},{"field":"stride"}],"root":"stepParameters"},"id":"Stride"}],"wireAction":"tick","wireAliases":[]}],"initializers":[{"id":"Initialize","inputs":[],"wireAction":"init","wireAliases":[]}],"interfaceVersion":"1.0.0","model":{"module":"Counter","source":"specs/Counter.tla"},"observations":[{"id":"Count","provenance":"implementation","wireName":"count"}],"schema":"mirrors.model-interface/v1","wire":{"actionVariable":"action_taken","parameterVariable":"parameters"}}"#;

fn config() -> ApalacheConfig {
    ApalacheConfig {
        spec_path: "Counter.tla".into(),
        init_predicate: None,
        next_predicate: None,
        const_init: Some("CInit".into()),
        invariant: "TraceComplete".into(),
        length_bound: 3,
        param_vars: Some("parameters".into()),
    }
}

fn metadata(digest: &str) -> GeneratedModelInterface {
    GeneratedModelInterface {
        semantic_digest: digest.into(),
        contract_json: CONTRACT.into(),
    }
}

fn first_reply(digest: &str) -> String {
    format!(
        r#"{{"proto_step":"spec_validated","result":"valid","modelInterface":{{"schema":"mirrors.model-interface-negotiation/v1","status":"matched","descriptorSchema":"mirrors.model-interface-descriptor/v1","semanticDigest":"sha256:{digest}"}}}}"#
    )
}

fn run_reply(
    first: String,
    configure: impl FnOnce(&mut CompiledAdapterSelection<'_>),
) -> (Result<(), Error>, Rc<RefCell<Calls>>) {
    let (_directory, path) = mirror_script(&[first]);
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut registry = registry(calls.clone(), false, false);
    let mut selection = selection(&mut registry);
    configure(&mut selection);
    let result = run_client_with_traces_negotiated_transport(
        spawn_fixture(&path),
        config(),
        vec!["counter.itf.json".into()],
        &mut selection,
    );
    (result, calls)
}

fn mirror_script(lines: &[String]) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("mirror-fixture");
    let mut script = String::from("#!/bin/sh\nIFS= read -r registration || exit 1\n");
    for (index, line) in lines.iter().enumerate() {
        script.push_str("printf '%s\\n' '");
        script.push_str(line);
        script.push_str("'\n");
        if index == 1 {
            script.push_str("IFS= read -r report || exit 0\n");
        }
    }
    let mut file = fs::File::create(&path).unwrap();
    file.write_all(script.as_bytes()).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    (directory, path)
}

fn spawn_fixture(path: &PathBuf) -> mirrorrust::Transport {
    for attempt in 0..10 {
        match spawn_mirror(path.to_str().unwrap()) {
            Ok(transport) => return transport,
            Err(Error::Io(error)) if error.raw_os_error() == Some(26) && attempt < 9 => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) => panic!("fixture spawn failed: {error}"),
        }
    }
    unreachable!("bounded fixture spawn loop returns or panics")
}

#[derive(Default)]
struct Calls {
    factory: usize,
    computer: usize,
    config: usize,
    dispose: usize,
}

fn selection<'a>(registry: &'a mut CompiledAdapterRegistry) -> CompiledAdapterSelection<'a> {
    CompiledAdapterSelection {
        metadata: metadata(DIGEST),
        adapter_id: "counter.fixture/v1".into(),
        target_profile: "mirrorrust-counter-fixture-v1".into(),
        state_computer_contract_version: STATE_COMPUTER_CONTRACT_VERSION.into(),
        registry,
        policy: NegotiationPolicy::Require,
        fallback_factory: None,
    }
}

fn registry(
    calls: Rc<RefCell<Calls>>,
    panic_compute: bool,
    cleanup_fails: bool,
) -> CompiledAdapterRegistry {
    let digest = SemanticDigest::from_hex(DIGEST).unwrap();
    CompiledAdapterRegistry::new(vec![CompiledAdapterRegistration {
        key: CompiledAdapterKey {
            semantic_digest: digest,
            adapter_id: "counter.fixture/v1".into(),
            target_profile: "mirrorrust-counter-fixture-v1".into(),
            state_computer_contract_version: STATE_COMPUTER_CONTRACT_VERSION.into(),
        },
        factory: Box::new(move |matched| {
            assert_eq!(matched.semantic_digest(), digest);
            calls.borrow_mut().factory += 1;
            let computer_calls = calls.clone();
            let config_calls = calls.clone();
            let dispose_calls = calls.clone();
            Ok(LocalBinding {
                semantic_digest: digest,
                computer: Box::new(move |_: &str, _: &State, _: &State| {
                    computer_calls.borrow_mut().computer += 1;
                    if panic_compute {
                        panic!("fixture callback panic")
                    }
                    Ok(State::new())
                }),
                assert_compatible_config: Box::new(move |candidate| {
                    config_calls.borrow_mut().config += 1;
                    if candidate.param_vars.as_deref() == Some("parameters") {
                        Ok(())
                    } else {
                        Err(BindingError::new(
                            "configuration_mismatch",
                            "Counter requires parameters",
                        ))
                    }
                }),
                dispose: Box::new(move || {
                    dispose_calls.borrow_mut().dispose += 1;
                    if cleanup_fails {
                        Err(BindingError::new("worker_cleanup", "cleanup failed"))
                    } else {
                        Ok(())
                    }
                }),
            })
        }),
    }])
}

fn adapter_key(digest: SemanticDigest) -> CompiledAdapterKey {
    CompiledAdapterKey {
        semantic_digest: digest,
        adapter_id: "counter.fixture/v1".into(),
        target_profile: "mirrorrust-counter-fixture-v1".into(),
        state_computer_contract_version: STATE_COMPUTER_CONTRACT_VERSION.into(),
    }
}

fn simple_registry(
    calls: Rc<RefCell<Calls>>,
    binding_digest: SemanticDigest,
    factory_fails: bool,
    config_fails: bool,
    cleanup_fails: bool,
) -> CompiledAdapterRegistry {
    let key_digest = SemanticDigest::from_hex(DIGEST).unwrap();
    CompiledAdapterRegistry::new(vec![CompiledAdapterRegistration {
        key: adapter_key(key_digest),
        factory: Box::new(move |_| {
            calls.borrow_mut().factory += 1;
            if factory_fails {
                return Err(BindingError::new("fixture_factory", "factory failed"));
            }
            let computer_calls = calls.clone();
            let config_calls = calls.clone();
            let dispose_calls = calls.clone();
            Ok(LocalBinding {
                semantic_digest: binding_digest,
                computer: Box::new(move |_: &str, _: &State, _: &State| {
                    computer_calls.borrow_mut().computer += 1;
                    Ok(State::new())
                }),
                assert_compatible_config: Box::new(move |_| {
                    config_calls.borrow_mut().config += 1;
                    if config_fails {
                        Err(BindingError::new("fixture_config", "config failed"))
                    } else {
                        Ok(())
                    }
                }),
                dispose: Box::new(move || {
                    dispose_calls.borrow_mut().dispose += 1;
                    if cleanup_fails {
                        Err(BindingError::new("fixture_cleanup", "cleanup failed"))
                    } else {
                        Ok(())
                    }
                }),
            })
        }),
    }])
}

#[test]
fn verify_metadata_is_strict_and_duplicate_aware() {
    let request = make_verify_request(&metadata(DIGEST), NegotiationPolicy::Require).unwrap();
    let _ = request;
    let duplicate = GeneratedModelInterface {
        semantic_digest: DIGEST.into(),
        contract_json:
            r#"{"schema":"mirrors.model-interface/v1","schema":"mirrors.model-interface/v1"}"#
                .into(),
    };
    assert!(make_verify_request(&duplicate, NegotiationPolicy::Require)
        .unwrap_err()
        .to_string()
        .contains("duplicate object key"));
    let mut unknown: serde_json::Value = serde_json::from_str(CONTRACT).unwrap();
    unknown
        .as_object_mut()
        .unwrap()
        .insert("typo".into(), true.into());
    let unknown = GeneratedModelInterface {
        semantic_digest: DIGEST.into(),
        contract_json: unknown.to_string(),
    };
    assert!(make_verify_request(&unknown, NegotiationPolicy::Require)
        .unwrap_err()
        .to_string()
        .contains("unknown field"));
}

#[test]
fn metadata_validates_recursive_closed_map_key_literals() {
    let mut contract: serde_json::Value = serde_json::from_str(CONTRACT).unwrap();
    contract["actions"][0]["inputs"][0]["from"]["path"] = serde_json::json!([{
        "mapKey": {
            "kind": "variant",
            "tag": "Some",
            "payload": {
                "kind": "map",
                "entries": [{
                    "key": {"kind":"int", "value":"123456789012345678901234567890"},
                    "value": {"kind":"record", "fields":[{
                        "name":"ok", "value":{"kind":"bool", "value":true}
                    }]}
                }]
            }
        }
    }]);
    let valid = GeneratedModelInterface {
        semantic_digest: DIGEST.into(),
        contract_json: contract.to_string(),
    };
    make_verify_request(&valid, NegotiationPolicy::Require).unwrap();

    contract["actions"][0]["inputs"][0]["from"]["path"][0]["mapKey"]["payload"]["entries"][0]
        ["key"]["value"] = "01".into();
    let invalid = GeneratedModelInterface {
        semantic_digest: DIGEST.into(),
        contract_json: contract.to_string(),
    };
    assert!(make_verify_request(&invalid, NegotiationPolicy::Require)
        .unwrap_err()
        .to_string()
        .contains("canonical decimal"));

    contract["actions"][0]["inputs"][0]["from"]["path"][0]["mapKey"] =
        serde_json::json!({"kind":"null", "extra":true});
    let open = GeneratedModelInterface {
        semantic_digest: DIGEST.into(),
        contract_json: contract.to_string(),
    };
    assert!(make_verify_request(&open, NegotiationPolicy::Require)
        .unwrap_err()
        .to_string()
        .contains("unknown field"));
}

#[test]
fn reply_parser_preserves_additive_numbers_private_key_objects_and_exact_depth() {
    let extension = format!(
        r#""proto_step":"spec_validated","result":"valid","modelInterface":{{"schema":"mirrors.model-interface-negotiation/v1","status":"matched","descriptorSchema":"mirrors.model-interface-descriptor/v1","semanticDigest":"sha256:{DIGEST}"}}"#
    );
    let accepted = format!(
        r#"{{{extension},"big":123456789012345678901234567890,"fraction":1.25,"exponent":1e400,"private":{{"$serde_json::private::Number":"1"}}}}"#
    );
    let lines = vec![
        accepted,
        r#"{"proto_step":"initial_state","action":"init","state":{}}"#.into(),
        r#"{"proto_step":"step_ok"}"#.into(),
        r#"{"proto_step":"all_steps_done"}"#.into(),
    ];
    let (_directory, path) = mirror_script(&lines);
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut registry = registry(calls, false, false);
    let mut selection = selection(&mut registry);
    let result = run_client_with_traces_negotiated_transport(
        spawn_fixture(&path),
        config(),
        vec!["counter.itf.json".into()],
        &mut selection,
    );
    assert!(result.is_ok(), "{result:?}");

    for (arrays, should_pass) in [(127, true), (128, false)] {
        let nested = format!("{}null{}", "[".repeat(arrays), "]".repeat(arrays));
        let reply = format!("{{{extension},\"extra\":{nested}}}");
        let (result, calls) = run_reply(reply, |_| {});
        let calls = calls.borrow();
        assert_eq!(
            calls.factory == 1,
            should_pass,
            "arrays={arrays}: {result:?}"
        );
        if !should_pass {
            assert_eq!((calls.computer, calls.dispose), (0, 0));
        }
    }
}

#[test]
fn prefer_fallback_validates_status_fields_before_factory() {
    let bad = format!(
        r#"{{"proto_step":"spec_validated","result":"valid","modelInterface":{{"schema":"mirrors.model-interface-negotiation/v1","status":"unsupported","semanticDigest":"sha256:{DIGEST}"}}}}"#
    );
    let (result, calls) = run_reply(bad, |selection| {
        selection.policy = NegotiationPolicy::Prefer;
        selection.fallback_factory = Some(Box::new(|_| panic!("fallback must not run")));
    });
    assert!(
        matches!(result, Err(Error::ModelInterface { ref code, .. }) if code == "negotiation_status_unexpected")
    );
    assert_eq!(calls.borrow().factory, 0);
}

#[test]
fn registration_failures_validate_all_digests_and_request_pin() {
    let cases = [
        format!(r#""actualSemanticDigest":12,"expectedSemanticDigest":"sha256:{DIGEST}""#),
        format!(
            r#""provenanceDigest":"SHA256:{DIGEST}","expectedSemanticDigest":"sha256:{DIGEST}""#
        ),
        format!(r#""expectedSemanticDigest":"sha256:{}""#, "b".repeat(64)),
        format!(r#""descriptorBytes":1.5,"expectedSemanticDigest":"sha256:{DIGEST}""#),
    ];
    for fields in cases {
        let reply = format!(
            r#"{{"proto_step":"register_error","error":"rejected","modelInterface":{{"schema":"mirrors.model-interface-negotiation/v1","status":"mismatch","code":"interface_digest_mismatch",{fields}}}}}"#
        );
        let (result, calls) = run_reply(reply, |_| {});
        assert!(result.is_err(), "malformed failure accepted");
        let calls = calls.borrow();
        assert_eq!((calls.factory, calls.computer, calls.dispose), (0, 0, 0));
    }
}

#[test]
fn structured_authorization_denial_is_registration_error_with_zero_callbacks() {
    let reply = r#"{"proto_step":"register_error","error":"adapter is not authorized","modelInterface":{"schema":"mirrors.model-interface-negotiation/v1","status":"unavailable","code":"authorization_denied"}}"#.to_string();
    let (result, calls) = run_reply(reply, |_| {});
    assert!(matches!(
        result,
        Err(Error::Registration { ref code, ref message })
            if code == "authorization_denied" && message == "adapter is not authorized"
    ));
    let calls = calls.borrow();
    assert_eq!(
        (calls.factory, calls.config, calls.computer, calls.dispose),
        (0, 0, 0, 0)
    );
}

#[test]
fn matched_reply_creates_and_disposes_exactly_one_binding() {
    let lines = vec![
        first_reply(DIGEST),
        r#"{"proto_step":"initial_state","action":"init","state":{}}"#.into(),
        r#"{"proto_step":"step_ok"}"#.into(),
        r#"{"proto_step":"all_steps_done"}"#.into(),
    ];
    let (_directory, path) = mirror_script(&lines);
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut registry = registry(calls.clone(), false, false);
    let mut selection = selection(&mut registry);
    let result = run_client_with_traces_negotiated_transport(
        spawn_fixture(&path),
        config(),
        vec!["counter.itf.json".into()],
        &mut selection,
    );
    assert!(result.is_ok(), "{result:?}");
    let calls = calls.borrow();
    assert_eq!(
        (calls.factory, calls.config, calls.computer, calls.dispose),
        (1, 1, 1, 1)
    );
}

#[test]
fn wrong_or_duplicate_match_runs_no_adapter_code() {
    for first in [
        first_reply(&"b".repeat(64)),
        first_reply(DIGEST).replacen(
            "\"status\":\"matched\"",
            "\"status\":\"matched\",\"status\":\"matched\"",
            1,
        ),
    ] {
        let (_directory, path) = mirror_script(&[first]);
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut registry = registry(calls.clone(), false, false);
        let mut selection = selection(&mut registry);
        let result = run_client_with_traces_negotiated_transport(
            spawn_fixture(&path),
            config(),
            vec!["counter.itf.json".into()],
            &mut selection,
        );
        assert!(result.is_err());
        let calls = calls.borrow();
        assert_eq!((calls.factory, calls.computer, calls.dispose), (0, 0, 0));
    }
}

#[test]
fn callback_panic_remains_primary_and_disposal_is_attempted_once() {
    let lines = vec![
        first_reply(DIGEST),
        r#"{"proto_step":"initial_state","action":"init","state":{}}"#.into(),
    ];
    let (_directory, path) = mirror_script(&lines);
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut registry = registry(calls.clone(), true, true);
    let mut selection = selection(&mut registry);
    let result = run_client_with_traces_negotiated_transport(
        spawn_fixture(&path),
        config(),
        vec!["counter.itf.json".into()],
        &mut selection,
    );
    assert!(
        matches!(result, Err(Error::ModelInterface { ref code, .. }) if code == "adapter_failure")
    );
    let calls = calls.borrow();
    assert_eq!((calls.factory, calls.computer, calls.dispose), (1, 1, 1));
}

#[test]
fn exact_registry_rejects_duplicate_and_unregistered_keys_without_factories() {
    let digest = SemanticDigest::from_hex(DIGEST).unwrap();
    let duplicate_calls = Rc::new(RefCell::new(Calls::default()));
    let registrations = (0..2)
        .map(|_| {
            let calls = duplicate_calls.clone();
            CompiledAdapterRegistration {
                key: adapter_key(digest),
                factory: Box::new(move |_| {
                    calls.borrow_mut().factory += 1;
                    Err(BindingError::new("unexpected", "must not run"))
                }),
            }
        })
        .collect();
    let mut duplicate = CompiledAdapterRegistry::new(registrations);
    let mut duplicate_selection = selection(&mut duplicate);
    let (_directory, path) = mirror_script(&[]);
    let result = run_client_with_traces_negotiated_transport(
        spawn_fixture(&path),
        config(),
        vec!["counter.itf.json".into()],
        &mut duplicate_selection,
    );
    assert!(
        matches!(result, Err(Error::ModelInterface { ref code, .. }) if code == "adapter_ambiguous")
    );
    assert_eq!(duplicate_calls.borrow().factory, 0);

    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut registry = registry(calls.clone(), false, false);
    let mut missing = selection(&mut registry);
    missing.adapter_id = "missing.fixture/v1".into();
    let (_directory, path) = mirror_script(&[]);
    let result = run_client_with_traces_negotiated_transport(
        spawn_fixture(&path),
        config(),
        vec!["counter.itf.json".into()],
        &mut missing,
    );
    assert!(
        matches!(result, Err(Error::ModelInterface { ref code, .. }) if code == "adapter_not_registered")
    );
    assert_eq!(calls.borrow().factory, 0);
}

#[test]
fn binding_validation_and_cleanup_keep_exact_lifetime_and_precedence() {
    let success_lines = vec![
        first_reply(DIGEST),
        r#"{"proto_step":"initial_state","action":"init","state":{}}"#.into(),
        r#"{"proto_step":"step_ok"}"#.into(),
        r#"{"proto_step":"all_steps_done"}"#.into(),
    ];
    let wrong_digest = SemanticDigest::from_hex(&"b".repeat(64)).unwrap();
    for (binding_digest, factory_fails, config_fails, cleanup_fails, code, expected) in [
        (
            wrong_digest,
            false,
            false,
            false,
            "binding_digest_mismatch",
            (1, 0, 0, 1),
        ),
        (
            SemanticDigest::from_hex(DIGEST).unwrap(),
            false,
            true,
            false,
            "binding_config_mismatch",
            (1, 1, 0, 1),
        ),
        (
            SemanticDigest::from_hex(DIGEST).unwrap(),
            true,
            false,
            false,
            "adapter_factory_failed",
            (1, 0, 0, 0),
        ),
        (
            SemanticDigest::from_hex(DIGEST).unwrap(),
            false,
            false,
            true,
            "adapter_dispose_failed",
            (1, 1, 1, 1),
        ),
    ] {
        let (_directory, path) = mirror_script(&success_lines);
        let calls = Rc::new(RefCell::new(Calls::default()));
        let mut registry = simple_registry(
            calls.clone(),
            binding_digest,
            factory_fails,
            config_fails,
            cleanup_fails,
        );
        let mut selection = selection(&mut registry);
        let result = run_client_with_traces_negotiated_transport(
            spawn_fixture(&path),
            config(),
            vec!["counter.itf.json".into()],
            &mut selection,
        );
        assert!(
            matches!(result, Err(Error::ModelInterface { code: ref actual, .. }) if actual == code),
            "{result:?}"
        );
        let calls = calls.borrow();
        assert_eq!(
            (calls.factory, calls.config, calls.computer, calls.dispose),
            expected
        );
    }
}

#[test]
fn prefer_requires_an_explicit_fresh_fallback_factory() {
    let legacy_reply = r#"{"proto_step":"spec_validated","result":"valid"}"#.to_string();
    let (missing, calls) = run_reply(legacy_reply.clone(), |selection| {
        selection.policy = NegotiationPolicy::Prefer;
    });
    assert!(
        matches!(missing, Err(Error::ModelInterface { ref code, .. }) if code == "legacy_fallback_unavailable")
    );
    assert_eq!(calls.borrow().factory, 0);

    let lines = vec![
        legacy_reply,
        r#"{"proto_step":"initial_state","action":"init","state":{}}"#.into(),
        r#"{"proto_step":"step_ok"}"#.into(),
        r#"{"proto_step":"all_steps_done"}"#.into(),
    ];
    let (_directory, path) = mirror_script(&lines);
    let calls = Rc::new(RefCell::new(Calls::default()));
    let mut registry = registry(calls.clone(), false, false);
    let fallback_calls = calls.clone();
    let digest = SemanticDigest::from_hex(DIGEST).unwrap();
    let mut selection = selection(&mut registry);
    selection.policy = NegotiationPolicy::Prefer;
    selection.fallback_factory = Some(Box::new(move |_| {
        fallback_calls.borrow_mut().factory += 1;
        let computer_calls = fallback_calls.clone();
        let config_calls = fallback_calls.clone();
        let dispose_calls = fallback_calls.clone();
        Ok(LocalBinding {
            semantic_digest: digest,
            computer: Box::new(move |_: &str, _: &State, _: &State| {
                computer_calls.borrow_mut().computer += 1;
                Ok(State::new())
            }),
            assert_compatible_config: Box::new(move |_| {
                config_calls.borrow_mut().config += 1;
                Ok(())
            }),
            dispose: Box::new(move || {
                dispose_calls.borrow_mut().dispose += 1;
                Ok(())
            }),
        })
    }));
    let result = run_client_with_traces_negotiated_transport(
        spawn_fixture(&path),
        config(),
        vec!["counter.itf.json".into()],
        &mut selection,
    );
    assert!(result.is_ok(), "{result:?}");
    let calls = calls.borrow();
    assert_eq!(
        (calls.factory, calls.config, calls.computer, calls.dispose),
        (1, 1, 1, 1)
    );
}
