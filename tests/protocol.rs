use mirrorrust::{as_int, as_record, as_str, encode_state, get_param, get_param_int, State, Value};
use mirrorrust::{preset_client, StateComputer};
use num_bigint::BigInt;
use serde_json::json;

fn st(pairs: Vec<(&str, Value)>) -> State {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

#[test]
fn encode_state_clean_itf() {
    let s = st(vec![
        ("count", Value::Int(BigInt::from(9007199254740991i64))),
        ("flag", Value::Bool(true)),
    ]);
    let encoded = encode_state(&s);
    assert_eq!(encoded["count"], json!({ "#bigint": "9007199254740991" }));
    assert_eq!(encoded["flag"], json!(true));
}

#[test]
fn encode_state_set_tuple_record_null() {
    let s = st(vec![
        ("items", Value::Set(vec![Value::Int(BigInt::from(1)), Value::Int(BigInt::from(2))])),
        ("pair", Value::Tuple(vec![Value::Str("foo".into()), Value::Int(BigInt::from(7))])),
        ("nothing", Value::Null),
        ("person", Value::Record(st(vec![("name", Value::Str("bob".into()))]))),
    ]);
    let e = encode_state(&s);
    assert_eq!(e["items"], json!({ "#set": [{ "#bigint": "1" }, { "#bigint": "2" }] }));
    assert_eq!(e["pair"], json!({ "#tup": ["foo", { "#bigint": "7" }] }));
    assert_eq!(e["nothing"], serde_json::Value::Null);
    assert_eq!(e["person"], json!({ "name": "bob" }));
}

#[test]
fn helper_as_int() {
    assert_eq!(as_int(&Value::Int(BigInt::from(42))), Some(&BigInt::from(42)));
    assert_eq!(as_int(&Value::Bool(true)), None);
    assert_eq!(as_int(&Value::Str("hi".into())), None);
    assert_eq!(as_int(&Value::Null), None);
}

#[test]
fn helper_as_str() {
    assert_eq!(as_str(&Value::Str("hello".into())), Some("hello"));
    assert_eq!(as_str(&Value::Int(BigInt::from(1))), None);
    assert_eq!(as_str(&Value::Bool(false)), None);
}

#[test]
fn helper_as_record() {
    let rec = st(vec![("a", Value::Int(BigInt::from(1)))]);
    assert_eq!(as_record(&Value::Record(rec.clone())), Some(&rec));
    assert_eq!(as_record(&Value::Int(BigInt::from(1))), None);
    assert_eq!(as_record(&Value::Null), None);
}

#[test]
fn helper_get_param_and_int() {
    let params = st(vec![
        ("x", Value::Record(st(vec![("foo", Value::Int(BigInt::from(42)))]))),
        ("y", Value::Int(BigInt::from(7))),
    ]);
    assert_eq!(
        get_param(&params, "x"),
        Some(&st(vec![("foo", Value::Int(BigInt::from(42)))]))
    );
    assert_eq!(get_param(&params, "y"), None);
    assert_eq!(get_param(&params, "missing"), None);
    assert_eq!(get_param_int(&params, "x", "foo"), 42);
    assert_eq!(get_param_int(&params, "missing", "foo"), 0);
    assert_eq!(get_param_int(&params, "x", "bar"), 0);
}

use mirrorrust::{
    encode_client_message, ApalacheConfig, ClientMessage, TraceGenerationConfig,
};

fn cfg() -> ApalacheConfig {
    ApalacheConfig {
        spec_path: "/foo/bar.tla".into(),
        init_predicate: None,
        next_predicate: None,
        const_init: None,
        invariant: "TraceComplete".into(),
        length_bound: 5,
        param_vars: None,
    }
}

#[test]
fn encode_register() {
    let msg = ClientMessage::Register {
        apalache_config: cfg(),
        trace_config: TraceGenerationConfig { num_traces: 10, view: None },
    };
    let v: serde_json::Value = serde_json::from_str(&encode_client_message(&msg)).unwrap();
    assert_eq!(v["proto_step"], json!("register"));
    assert_eq!(v["apalacheConfig"]["specPath"], json!("/foo/bar.tla"));
    assert_eq!(v["apalacheConfig"]["invariant"], json!("TraceComplete"));
    assert_eq!(v["apalacheConfig"]["lengthBound"], json!(5));
    assert_eq!(v["traceConfig"]["numTraces"], json!(10));
    assert!(v["apalacheConfig"].get("initPredicate").is_none());
    assert!(v["apalacheConfig"].get("constInit").is_none());
    assert!(v["traceConfig"].get("view").is_none());
}

#[test]
fn encode_register_traces() {
    let msg = ClientMessage::RegisterTraces {
        apalache_config: cfg(),
        itf_trace_paths: vec!["/tmp/trace1.itf.json".into(), "/tmp/trace2.itf.json".into()],
    };
    let v: serde_json::Value = serde_json::from_str(&encode_client_message(&msg)).unwrap();
    assert_eq!(v["proto_step"], json!("register_traces"));
    assert_eq!(v["itfTracePaths"], json!(["/tmp/trace1.itf.json", "/tmp/trace2.itf.json"]));
}

#[test]
fn encode_report_state_is_tagged_with_bigint() {
    let s = st(vec![
        ("count", Value::Int(BigInt::parse_bytes(b"9007199254740991", 10).unwrap())),
        ("flag", Value::Bool(true)),
    ]);
    let msg = ClientMessage::ReportState { state: s };
    let v: serde_json::Value = serde_json::from_str(&encode_client_message(&msg)).unwrap();
    assert_eq!(
        v["state"]["count"],
        json!({ "tag": "int", "val": { "#bigint": "9007199254740991" } })
    );
    assert_eq!(v["state"]["flag"], json!({ "tag": "bool", "val": true }));
}

use mirrorrust::{decode_mirror_message, MirrorMessage, SpecResult};

#[test]
fn decode_spec_validated_valid() {
    let m = decode_mirror_message(r#"{"proto_step":"spec_validated","result":"valid"}"#).unwrap();
    assert_eq!(m, MirrorMessage::SpecValidated { result: SpecResult::Valid });
}

#[test]
fn decode_spec_validated_invalid() {
    let m = decode_mirror_message(r#"{"proto_step":"spec_validated","result":{"invalid":"bad spec"}}"#).unwrap();
    assert_eq!(
        m,
        MirrorMessage::SpecValidated {
            result: SpecResult::Invalid(r#"{"invalid":"bad spec"}"#.to_string())
        }
    );
}

#[test]
fn decode_initial_state() {
    let m = decode_mirror_message(r#"{"proto_step":"initial_state","action":"Init","state":{"count":0}}"#).unwrap();
    assert_eq!(
        m,
        MirrorMessage::InitialState {
            action: "Init".into(),
            state: st(vec![("count", Value::Int(BigInt::from(0)))]),
        }
    );
}

#[test]
fn decode_next_step() {
    let m = decode_mirror_message(r#"{"proto_step":"next_step","action":"Incr","parameters":{"by":1}}"#).unwrap();
    assert_eq!(
        m,
        MirrorMessage::NextStep {
            action: "Incr".into(),
            parameters: st(vec![("by", Value::Int(BigInt::from(1)))]),
        }
    );
}

#[test]
fn decode_step_ok_and_all_done() {
    assert_eq!(decode_mirror_message(r#"{"proto_step":"step_ok"}"#).unwrap(), MirrorMessage::StepOk);
    assert_eq!(decode_mirror_message(r#"{"proto_step":"all_steps_done"}"#).unwrap(), MirrorMessage::AllStepsDone);
}

#[test]
fn decode_step_mismatch() {
    let m = decode_mirror_message(r#"{"proto_step":"step_mismatch","action":"Inc","expected":{"count":1},"actual":{"count":2}}"#).unwrap();
    assert_eq!(
        m,
        MirrorMessage::StepMismatch {
            action: Some("Inc".into()),
            expected: st(vec![("count", Value::Int(BigInt::from(1)))]),
            actual: st(vec![("count", Value::Int(BigInt::from(2)))]),
        }
    );
}

#[test]
fn decode_gen_traces_done_and_errors() {
    assert_eq!(
        decode_mirror_message(r#"{"proto_step":"gen_traces_done","itfTracePaths":["/tmp/t1.itf.json"]}"#).unwrap(),
        MirrorMessage::GenTracesDone { itf_trace_paths: vec!["/tmp/t1.itf.json".into()] }
    );
    assert_eq!(
        decode_mirror_message(r#"{"proto_step":"protocol_error","error":"bad!"}"#).unwrap(),
        MirrorMessage::ProtocolError { error: "bad!".into() }
    );
    assert_eq!(
        decode_mirror_message(r#"{"proto_step":"register_error","error":"spec not found"}"#).unwrap(),
        MirrorMessage::RegisterError { error: "spec not found".into() }
    );
}

#[test]
fn decode_unknown_proto_step() {
    assert_eq!(
        decode_mirror_message(r#"{"proto_step":"unknown_thing","x":1}"#).unwrap(),
        MirrorMessage::ProtocolError { error: "unknown proto_step: unknown_thing".into() }
    );
}

#[test]
fn decode_value_kinds_via_initial_state() {
    let m = decode_mirror_message(
        r##"{"proto_step":"initial_state","action":"Init","state":{
            "ready":true,"done":false,"name":"alice","nothing":null,
            "person":{"name":"bob","age":30},
            "pair":{"#tup":["foo",7]},
            "items":[1,2,3]
        }}"##,
    )
    .unwrap();
    let state = match m {
        MirrorMessage::InitialState { state, .. } => state,
        _ => panic!("expected initial_state"),
    };
    assert_eq!(state["ready"], Value::Bool(true));
    assert_eq!(state["done"], Value::Bool(false));
    assert_eq!(state["name"], Value::Str("alice".into()));
    assert_eq!(state["nothing"], Value::Null);
    assert_eq!(
        state["person"],
        Value::Record(st(vec![("name", Value::Str("bob".into())), ("age", Value::Int(BigInt::from(30)))]))
    );
    assert_eq!(
        state["pair"],
        Value::Tuple(vec![Value::Str("foo".into()), Value::Int(BigInt::from(7))])
    );
    assert_eq!(
        state["items"],
        Value::Set(vec![Value::Int(BigInt::from(1)), Value::Int(BigInt::from(2)), Value::Int(BigInt::from(3))])
    );
}

#[test]
fn encode_state_decode_round_trip_bigint() {
    let s = st(vec![("count", Value::Int(BigInt::parse_bytes(b"12345678901234567890", 10).unwrap()))]);
    let line = serde_json::to_string(&json!({
        "proto_step": "initial_state",
        "action": "Init",
        "state": encode_state(&s),
    }))
    .unwrap();
    let m = decode_mirror_message(&line).unwrap();
    let state = match m {
        MirrorMessage::InitialState { state, .. } => state,
        _ => panic!(),
    };
    assert_eq!(state["count"], Value::Int(BigInt::parse_bytes(b"12345678901234567890", 10).unwrap()));
}

#[test]
fn preset_client_serves_in_order() {
    let s0 = st(vec![("count", Value::Int(BigInt::from(0)))]);
    let s1 = st(vec![("count", Value::Int(BigInt::from(2)))]);
    let mut pc = preset_client(vec![s0.clone(), s1.clone()]);
    let empty = State::new();
    assert_eq!(pc.compute("init", &empty, &empty), s0);
    assert_eq!(pc.compute("tick", &empty, &s0), s1);
}

#[test]
#[should_panic(expected = "preset_client exhausted")]
fn preset_client_panics_when_exhausted() {
    let mut pc = preset_client(vec![]);
    let empty = State::new();
    let _ = pc.compute("init", &empty, &empty);
}
