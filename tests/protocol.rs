use mirrorrust::{as_int, as_record, as_str, encode_state, get_param, get_param_int, State, Value};
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
