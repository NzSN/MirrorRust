use mirrorrust::{decode_mirror_message, encode_client_message, ClientMessage, MirrorMessage};
use serde_json::Value;
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    std::env::var_os("MIRRORS_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("../Mirrors/test/fixtures"))
}

fn json_lines(name: &str) -> Vec<Value> {
    std::fs::read_to_string(fixtures().join(name))
        .unwrap_or_else(|error| panic!("canonical fixture {name} is unavailable: {error}"))
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn supported_client_step(step: &str) -> bool {
    matches!(
        step,
        "register"
            | "register_traces"
            | "register_trace_gen"
            | "register_validate"
            | "register_validate_async"
            | "register_trace_gen_async"
            | "query_job"
            | "await_job"
            | "cancel_job"
            | "report_state"
    )
}

fn supported_mirror_step(step: &str) -> bool {
    matches!(
        step,
        "spec_validated"
            | "initial_state"
            | "next_step"
            | "step_ok"
            | "step_mismatch"
            | "all_steps_done"
            | "gen_traces_done"
            | "protocol_error"
            | "register_error"
            | "job_accepted"
            | "job_status"
            | "job_result"
    )
}

fn normalize_optional_nulls(mut value: Value) -> Value {
    let Some(message) = value.as_object_mut() else {
        return value;
    };
    for key in ["spec", "destPath", "timeoutSecs"] {
        if message.get(key).is_some_and(Value::is_null) {
            message.remove(key);
        }
    }
    if let Some(config) = message
        .get_mut("apalacheConfig")
        .and_then(Value::as_object_mut)
    {
        config.retain(|_, value| !value.is_null());
    }
    if let Some(config) = message
        .get_mut("traceConfig")
        .and_then(Value::as_object_mut)
    {
        if config.get("view").is_some_and(Value::is_null) {
            config.remove("view");
        }
    }
    value
}

#[test]
fn supported_client_messages_round_trip_the_canonical_wire_corpus() {
    for expected in json_lines("client_messages.jsonl") {
        let step = expected["proto_step"].as_str().unwrap().to_string();
        if !supported_client_step(&step) {
            continue;
        }
        let message: ClientMessage = serde_json::from_value(expected.clone()).unwrap();
        let encoded: Value = serde_json::from_str(&encode_client_message(&message)).unwrap();
        assert_eq!(
            normalize_optional_nulls(encoded),
            normalize_optional_nulls(expected),
            "canonical {step} shape changed"
        );
    }
}

#[test]
fn supported_mirror_messages_decode_the_canonical_wire_corpus() {
    for expected in json_lines("mirror_messages.jsonl") {
        let step = expected["proto_step"].as_str().unwrap();
        if !supported_mirror_step(step) {
            continue;
        }
        let decoded = decode_mirror_message(&expected.to_string()).unwrap();
        assert!(
            !matches!(decoded, MirrorMessage::ProtocolError { .. }) || step == "protocol_error",
            "canonical {step} failed to decode: {decoded:?}"
        );
    }
}
